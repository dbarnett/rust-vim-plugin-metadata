use crate::data::VimModule;
use crate::{Error, VimNode, VimPlugin};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::{fs, str};
use tree_sitter::{Parser, Point};
use treenodes::TreeNodeMetadata;
use walkdir::WalkDir;

mod treenodes;

// TODO: Also support "after" equivalents.
const DEFAULT_SECTION_ORDER: [&str; 9] = [
    "plugin", "instant", "autoload", "syntax", "indent", "ftdetect", "ftplugin", "spell", "colors",
];

#[derive(Default)]
pub struct VimParser {
    parser: Parser,
}

impl VimParser {
    pub fn new() -> crate::Result<Self> {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_vim::language())?;
        Ok(Self { parser })
    }

    /// Parses all supported metadata from a single plugin at the given path.
    pub fn parse_plugin_dir<P: AsRef<Path> + Copy>(&mut self, path: P) -> crate::Result<VimPlugin> {
        let mut modules_for_sections: HashMap<String, Vec<VimModule>> = HashMap::new();
        let sections_to_include = HashSet::from(DEFAULT_SECTION_ORDER);
        for entry in WalkDir::new(path) {
            let entry = entry?;
            if !(entry.file_type().is_file()
                && entry.file_name().to_string_lossy().ends_with(".vim"))
            {
                continue;
            }
            let relative_path = entry.path().strip_prefix(path).unwrap();
            let section_name = relative_path
                .iter()
                .nth(0)
                .expect("path should be a strict prefix of path under it")
                .to_string_lossy();
            if !sections_to_include.contains(section_name.as_ref()) {
                continue;
            }
            let module = self.parse_module_file(entry.path())?;
            // Replace absolute path with one relative to plugin root.
            let module = VimModule {
                path: relative_path.to_owned().into(),
                ..module
            };
            modules_for_sections
                .entry(section_name.into())
                .or_default()
                .push(module);
        }
        let modules = DEFAULT_SECTION_ORDER
            .iter()
            .flat_map(|section_name| {
                modules_for_sections
                    .remove(*section_name)
                    .unwrap_or_default()
            })
            .collect();
        Ok(VimPlugin { content: modules })
    }

    /// Parses and returns metadata for a single module (a.k.a. file) of vimscript code.
    pub fn parse_module_file<P: AsRef<Path>>(&mut self, path: P) -> crate::Result<VimModule> {
        let code = fs::read_to_string(path.as_ref())?;
        let module = self.parse_module_str(&code)?;
        Ok(VimModule {
            path: Some(path.as_ref().to_owned()),
            ..module
        })
    }

    /// Parses and returns metadata for a single module (a.k.a. file) of vimscript code.
    pub fn parse_module_str(&mut self, code: &str) -> crate::Result<VimModule> {
        let tree = self.parser.parse(code, None).ok_or(Error::ParsingFailure)?;
        let mut tree_cursor = tree.walk();
        let mut module_nodes: Vec<VimNode> = Vec::new();
        let mut module_doc = None;
        let mut last_block_comment: Option<TreeNodeMetadata> = None;
        let mut reached_end = !tree_cursor.goto_first_child();
        while !reached_end {
            let mut node_metadata: TreeNodeMetadata = (tree_cursor.node(), code.as_bytes()).into();
            let cur_pos = tree_cursor.node().start_position();
            let mut next_pos = Point {
                row: cur_pos.row + 1,
                ..cur_pos
            };
            if node_metadata.kind() == "comment" {
                // Consume more lines of comment.
                loop {
                    match tree_cursor.node().next_sibling() {
                        Some(s) if s.kind() == "comment" && s.start_position() == next_pos => {
                            // Another comment at same indentation on the following line.
                            // Consume and absorb into node_metadata.
                            next_pos = Point {
                                row: next_pos.row + 1,
                                ..next_pos
                            };
                            tree_cursor.goto_next_sibling();
                            node_metadata.treenodes.push(tree_cursor.node());
                        }
                        _ => {
                            break;
                        }
                    }
                }
            }
            node_metadata.maybe_consume_doc(&mut last_block_comment);
            reached_end = !tree_cursor.goto_next_sibling();

            // Consume any dangling comments that can no longer attach to any node after.
            let mut nodes_to_consume = vec![];
            if let Some(last) = last_block_comment.take() {
                nodes_to_consume.push(last);
            }
            if node_metadata.kind() != "comment"
                || tree_cursor.node().start_position() != next_pos
                || reached_end
            {
                nodes_to_consume.push(node_metadata);
            } else {
                last_block_comment = Some(node_metadata);
            }
            let mut comment_can_be_module_doc = module_doc.is_none() && module_nodes.is_empty();
            for node_metadata in nodes_to_consume {
                for node in <TreeNodeMetadata<'_> as Into<Vec<_>>>::into(node_metadata) {
                    match node {
                        VimNode::StandaloneDocComment(doc_content) if comment_can_be_module_doc => {
                            // This standalone doc comment is the first one in the module.
                            // Treat it as overall module doc.
                            module_doc = Some(doc_content);
                            comment_can_be_module_doc = false;
                        }
                        node => {
                            module_nodes.push(node);
                        }
                    }
                }
            }
        }
        Ok(VimModule {
            path: None,
            doc: module_doc,
            nodes: module_nodes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn parse_module_empty() {
        let mut parser = VimParser::new().unwrap();
        assert_eq!(
            parser.parse_module_str("").unwrap(),
            VimModule {
                path: None,
                doc: None,
                nodes: vec![]
            }
        );
    }

    #[test]
    fn parse_module_one_nondoc_comment() {
        let mut parser = VimParser::new().unwrap();
        assert_eq!(
            parser.parse_module_str("\" A comment").unwrap(),
            VimModule {
                path: None,
                doc: None,
                nodes: vec![]
            }
        );
    }

    #[test]
    fn parse_module_one_doc() {
        let code = r#"
""
" Foo
"#;
        let mut parser = VimParser::new().unwrap();
        assert_eq!(
            parser.parse_module_str(code).unwrap(),
            VimModule {
                path: None,
                doc: "Foo".to_string().into(),
                nodes: vec![]
            }
        );
    }

    #[test]
    fn parse_module_messy_multiline_doc() {
        let code = r#"
"" Foo
"bar
"#;
        let mut parser = VimParser::new().unwrap();
        assert_eq!(
            parser.parse_module_str(code).unwrap(),
            VimModule {
                path: None,
                doc: "Foo\nbar".to_string().into(),
                nodes: vec![]
            }
        );
    }

    #[test]
    fn parse_module_adjacent_docs() {
        let code = r#"
""
" Doc comment.
""
" More doc comment.
"#;
        let mut parser = VimParser::new().unwrap();
        assert_eq!(
            parser.parse_module_str(code).unwrap(),
            VimModule {
                path: None,
                doc: Some("Doc comment.\n\"\nMore doc comment.".into()),
                nodes: vec![],
            },
        );
    }

    #[test]
    fn parse_module_doc_before_statement() {
        let code = r#"
""
" Actually a file header.
echo 'Hi'
func MyFunc() | endfunc
"#;
        let mut parser = VimParser::new().unwrap();
        assert_eq!(
            parser.parse_module_str(code).unwrap(),
            VimModule {
                path: None,
                doc: "Actually a file header.".to_string().into(),
                nodes: vec![
                    // Note: echo statement doesn't produce any nodes.
                    VimNode::Function {
                        name: "MyFunc".into(),
                        args: vec![],
                        modifiers: vec![],
                        doc: None,
                    }
                ],
            }
        );
    }

    #[test]
    fn parse_module_bare_function() {
        let code = r#"
func MyFunc()
  return 1
endfunc
"#;
        let mut parser = VimParser::new().unwrap();
        assert_eq!(
            parser.parse_module_str(code).unwrap(),
            VimModule {
                path: None,
                doc: None,
                nodes: vec![VimNode::Function {
                    name: "MyFunc".into(),
                    args: vec![],
                    modifiers: vec![],
                    doc: None
                }]
            }
        );
    }

    #[test]
    fn parse_module_doc_and_function() {
        let code = r#"
""
" Does a thing.
"
" Call and enjoy.
func MyFunc()
  return 1
endfunc
"#;
        let mut parser = VimParser::new().unwrap();
        assert_eq!(
            parser.parse_module_str(code).unwrap(),
            VimModule {
                path: None,
                doc: None,
                nodes: vec![VimNode::Function {
                    name: "MyFunc".into(),
                    args: vec![],
                    modifiers: vec![],
                    doc: Some("Does a thing.\n\nCall and enjoy.".into()),
                }]
            }
        );
    }

    #[test]
    fn parse_module_func_with_args() {
        let code = r#"
func MyFunc(arg1, arg2)
  return 1
endfunc
"#;
        let mut parser = VimParser::new().unwrap();
        assert_eq!(
            parser.parse_module_str(code).unwrap(),
            VimModule {
                path: None,
                doc: None,
                nodes: vec![VimNode::Function {
                    name: "MyFunc".into(),
                    args: vec!["arg1".into(), "arg2".into()],
                    modifiers: vec![],
                    doc: None
                }]
            }
        );
    }

    #[test]
    fn parse_module_func_with_opt_args_and_modifiers() {
        let code = r#"
func! MyFunc(arg1, ...) range dict abort
  return 1
endfunc
"#;
        let mut parser = VimParser::new().unwrap();
        assert_eq!(
            parser.parse_module_str(code).unwrap(),
            VimModule {
                path: None,
                doc: None,
                nodes: vec![VimNode::Function {
                    name: "MyFunc".into(),
                    args: vec!["arg1".into(), "...".into()],
                    modifiers: vec!["!".into(), "range".into(), "dict".into(), "abort".into()],
                    doc: None
                }]
            }
        );
    }

    #[test]
    fn parse_module_two_docs() {
        let code = r#"
"" One doc

"" Another doc
"#;
        let mut parser = VimParser::new().unwrap();
        assert_eq!(
            parser.parse_module_str(code).unwrap(),
            VimModule {
                path: None,
                doc: Some("One doc".into()),
                nodes: vec![VimNode::StandaloneDocComment("Another doc".into()),]
            }
        );
    }

    #[test]
    fn parse_module_comment_then_doc() {
        let code = r#"
" Normal comment

""
" Module doc
"#;
        let mut parser = VimParser::new().unwrap();
        assert_eq!(
            parser.parse_module_str(code).unwrap(),
            VimModule {
                path: None,
                doc: Some("Module doc".into()),
                nodes: vec![]
            }
        );
    }

    #[test]
    fn parse_module_different_doc_indentations() {
        let code = r#"
"" One doc
 " Ignored comment
"#;
        let mut parser = VimParser::new().unwrap();
        assert_eq!(
            parser.parse_module_str(code).unwrap(),
            VimModule {
                path: None,
                doc: Some("One doc".into()),
                nodes: vec![
                    // Comment at different indentation is treated as a normal
                    // non-doc comment and ignored.
                ],
            }
        );
    }

    #[test]
    fn parse_module_two_funcs() {
        let code = r#"func FuncOne() | endfunc
func FuncTwo() | endfunc"#;
        let mut parser = VimParser::new().unwrap();
        assert_eq!(
            parser.parse_module_str(code).unwrap(),
            VimModule {
                path: None,
                doc: None,
                nodes: vec![
                    VimNode::Function {
                        name: "FuncOne".into(),
                        args: vec![],
                        modifiers: vec![],
                        doc: None
                    },
                    VimNode::Function {
                        name: "FuncTwo".into(),
                        args: vec![],
                        modifiers: vec![],
                        doc: None
                    },
                ]
            }
        );
    }

    #[test]
    fn parse_module_autoload_funcname() {
        let code = "func foo#bar#Baz() | endfunc";
        let mut parser = VimParser::new().unwrap();
        assert_eq!(
            parser.parse_module_str(code).unwrap(),
            VimModule {
                path: None,
                doc: None,
                nodes: vec![VimNode::Function {
                    name: "foo#bar#Baz".into(),
                    args: vec![],
                    modifiers: vec![],
                    doc: None
                }]
            }
        );
    }

    #[test]
    fn parse_module_scriptlocal_funcname() {
        let code = "func s:SomeFunc() | endfunc";
        let mut parser = VimParser::new().unwrap();
        assert_eq!(
            parser.parse_module_str(code).unwrap(),
            VimModule {
                path: None,
                doc: None,
                nodes: vec![VimNode::Function {
                    name: "s:SomeFunc".into(),
                    args: vec![],
                    modifiers: vec![],
                    doc: None
                }]
            }
        );
    }

    #[test]
    fn parse_module_nested_func() {
        let code = r#"
function Outer()
  let l:thing = {}
  function l:thing.Inner()
    return 1
  endfunction
  return l:thing
endfunction
"#;
        let mut parser = VimParser::new().unwrap();
        assert_eq!(
            parser.parse_module_str(code).unwrap(),
            VimModule {
                path: None,
                doc: None,
                nodes: vec![
                    VimNode::Function {
                        name: "Outer".into(),
                        args: vec![],
                        modifiers: vec![],
                        doc: None
                    },
                    // TODO: Should have more nodes for inner function.
                ]
            }
        );
    }

    #[test]
    fn parse_module_one_command() {
        let code = r#"command SomeCommand echo "Hi""#;
        let mut parser = VimParser::new().unwrap();
        assert_eq!(
            parser.parse_module_str(code).unwrap(),
            VimModule {
                path: None,
                doc: None,
                nodes: vec![VimNode::Command {
                    name: "SomeCommand".into(),
                    modifiers: vec![],
                    doc: None
                }],
            }
        );
    }

    #[test]
    fn parse_module_command_with_doc_and_modifiers() {
        let code = r#"
""
" Do a complex thing.
command -range -bang -nargs=+ -bar SomeComplexCommand call SomeHelper() | echo 'Hi'
"#;
        let mut parser = VimParser::new().unwrap();
        assert_eq!(
            parser.parse_module_str(code).unwrap(),
            VimModule {
                path: None,
                doc: None,
                nodes: vec![VimNode::Command {
                    name: "SomeComplexCommand".into(),
                    modifiers: vec![
                        "-range".into(),
                        "-bang".into(),
                        "-nargs=+".into(),
                        "-bar".into()
                    ],
                    doc: Some("Do a complex thing.".into()),
                }],
            }
        );
    }

    #[test]
    fn parse_module_one_variable() {
        let code = "let somevar = 1";
        let mut parser = VimParser::new().unwrap();
        assert_eq!(
            parser.parse_module_str(code).unwrap(),
            VimModule {
                path: None,
                doc: None,
                nodes: vec![VimNode::Variable {
                    name: "somevar".into(),
                    init_value_token: "1".into(),
                    doc: None,
                }],
            },
        );
    }

    #[test]
    fn parse_module_variables_with_doc() {
        let code = r#"
""
" Doc for first variable.
let g:somevar = 'xyz' | let s:othervar = system("ls")
"#;
        let mut parser = VimParser::new().unwrap();
        assert_eq!(
            parser.parse_module_str(code).unwrap(),
            VimModule {
                path: None,
                doc: None,
                nodes: vec![
                    VimNode::Variable {
                        name: "g:somevar".into(),
                        init_value_token: "'xyz'".into(),
                        doc: Some("Doc for first variable.".into()),
                    },
                    VimNode::Variable {
                        name: "s:othervar".into(),
                        init_value_token: "system(\"ls\")".into(),
                        doc: None,
                    },
                ],
            },
        );
    }

    #[test]
    fn parse_module_one_flag() {
        let code = "call Flag('someflag', 'somedefault')";
        let mut parser = VimParser::new().unwrap();
        assert_eq!(
            parser.parse_module_str(code).unwrap(),
            VimModule {
                path: None,
                doc: None,
                nodes: vec![VimNode::Flag {
                    name: "someflag".into(),
                    default_value_token: Some("'somedefault'".into()),
                    doc: None
                }],
            }
        );
    }

    #[test]
    fn parse_module_flag_without_default() {
        let code = "call Flag('someflag')";
        let mut parser = VimParser::new().unwrap();
        assert_eq!(
            parser.parse_module_str(code).unwrap(),
            VimModule {
                path: None,
                doc: None,
                nodes: vec![VimNode::Flag {
                    name: "someflag".into(),
                    default_value_token: None,
                    doc: None
                }],
            }
        );
    }

    #[test]
    fn parse_module_flag_with_doc() {
        let code = r#"
""
" A flag for the value of a thing.
call Flag('someflag', 'somedefault')
"#;
        let mut parser = VimParser::new().unwrap();
        assert_eq!(
            parser.parse_module_str(code).unwrap(),
            VimModule {
                path: None,
                doc: None,
                nodes: vec![VimNode::Flag {
                    name: "someflag".into(),
                    default_value_token: Some("'somedefault'".into()),
                    doc: Some("A flag for the value of a thing.".into()),
                }],
            }
        );
    }

    #[test]
    fn parse_module_flag_s_plugin() {
        let code = r#"
let [s:plugin, s:enter] = plugin#Enter(expand('<sfile>:p'))
if !s:enter
  finish
endif
call s:plugin.Flag('someflag', 'somedefault')
"#;
        let mut parser = VimParser::new().unwrap();
        assert_eq!(
            parser.parse_module_str(code).unwrap(),
            VimModule {
                path: None,
                doc: None,
                nodes: vec![
                    VimNode::Variable {
                        name: "s:plugin".into(),
                        init_value_token: "plugin#Enter(expand('<sfile>:p'))[0]".into(),
                        doc: None,
                    },
                    VimNode::Variable {
                        name: "s:enter".into(),
                        init_value_token: "plugin#Enter(expand('<sfile>:p'))[1]".into(),
                        doc: None,
                    },
                    VimNode::Flag {
                        name: "someflag".into(),
                        default_value_token: Some("'somedefault'".into()),
                        doc: None
                    },
                ],
            }
        );
    }

    #[test]
    fn parse_module_flag_name_special_chars() {
        let code = r#"call Flag("some\"'flag֎")"#;
        let mut parser = VimParser::new().unwrap();
        assert_eq!(
            parser.parse_module_str(code).unwrap(),
            VimModule {
                path: None,
                doc: None,
                nodes: vec![VimNode::Flag {
                    name: r#"some"'flag֎"#.into(),
                    default_value_token: None,
                    doc: None
                }],
            }
        );
    }

    #[test]
    fn parse_module_comment_and_call() {
        let code = r#"
" Some normal comment.
call SomeFunc()
"#;
        let mut parser = VimParser::new().unwrap();
        assert_eq!(
            parser.parse_module_str(code).unwrap(),
            VimModule {
                path: None,
                doc: None,
                nodes: vec![],
            }
        );
    }

    #[test]
    fn parse_module_unicode() {
        let code = r#"
""
" Fun stuff 🎈 ( ͡° ͜ʖ ͡°)
"#;
        let mut parser = VimParser::new().unwrap();
        assert_eq!(
            parser.parse_module_str(code).unwrap(),
            VimModule {
                path: None,
                doc: Some("Fun stuff 🎈 ( ͡° ͜ʖ ͡°)".into()),
                nodes: vec![],
            }
        );
    }

    #[test]
    fn parse_plugin_dir_empty() {
        let mut parser = VimParser::new().unwrap();
        let tmp_dir = tempdir().unwrap();
        let plugin = parser.parse_plugin_dir(tmp_dir.path()).unwrap();
        assert_eq!(plugin, VimPlugin { content: vec![] });
    }

    #[test]
    fn parse_plugin_dir_one_autoload_func() {
        let mut parser = VimParser::new().unwrap();
        let tmp_dir = tempdir().unwrap();
        create_plugin_file(
            tmp_dir.path(),
            "autoload/foo.vim",
            r#"
func foo#Bar()
  sleep 1
endfunc
"#,
        );
        let plugin = parser.parse_plugin_dir(tmp_dir.path()).unwrap();
        assert_eq!(
            plugin,
            VimPlugin {
                content: vec![VimModule {
                    path: PathBuf::from("autoload/foo.vim").into(),
                    doc: None,
                    nodes: vec![VimNode::Function {
                        name: "foo#Bar".into(),
                        args: vec![],
                        modifiers: vec![],
                        doc: None
                    }]
                }],
            }
        );
    }

    #[test]
    fn parse_plugin_dir_various_subdirs() {
        let mut parser = VimParser::new().unwrap();
        let tmp_dir = tempdir().unwrap();
        create_plugin_file(tmp_dir.path(), "ignored_not_in_subdir.vim", "");
        create_plugin_file(tmp_dir.path(), "autoload/x.vim", "");
        create_plugin_file(tmp_dir.path(), "instant/x.vim", "");
        create_plugin_file(tmp_dir.path(), "plugin/x.vim", "");
        create_plugin_file(tmp_dir.path(), "colors/x.vim", "");
        create_plugin_file(tmp_dir.path(), "spell/x.vim", "");
        assert_eq!(
            parser.parse_plugin_dir(tmp_dir.path()).unwrap(),
            VimPlugin {
                content: vec![
                    VimModule {
                        path: PathBuf::from("plugin/x.vim").into(),
                        doc: None,
                        nodes: vec![],
                    },
                    VimModule {
                        path: PathBuf::from("instant/x.vim").into(),
                        doc: None,
                        nodes: vec![],
                    },
                    VimModule {
                        path: PathBuf::from("autoload/x.vim").into(),
                        doc: None,
                        nodes: vec![],
                    },
                    VimModule {
                        path: PathBuf::from("spell/x.vim").into(),
                        doc: None,
                        nodes: vec![],
                    },
                    VimModule {
                        path: PathBuf::from("colors/x.vim").into(),
                        doc: None,
                        nodes: vec![],
                    },
                ]
            }
        );
    }

    fn create_plugin_file<P: AsRef<Path>>(root: &Path, subpath: P, contents: &str) {
        let filepath = root.join(subpath);
        fs::create_dir_all(filepath.parent().unwrap()).unwrap();
        fs::write(filepath, contents).unwrap()
    }
}
