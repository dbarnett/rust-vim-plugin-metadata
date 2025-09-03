# vim-plugin-metadata 

Parse and analyze your vim plugins, from Rust or Python!

WARNING: This library is early alpha, still missing tons of functionality, and probably has serious
bugs. Use at your own risk.

## Usage

Install [in Rust](https://crates.io/crates/vim-plugin-metadata) or [in
Python](https://pypi.org/project/vim-plugin-metadata/), point it at a plugin directory, get
metadata.

Rust:
```rust
use vim_plugin_metadata::VimParser;

fn main() {
    let mut parser = VimParser::new().unwrap();
    let plugin = parser.parse_plugin_dir(".vim/plugged/someplugin").unwrap();
    println!("{plugin:#?}");
}
```
```
VimPlugin {
    content: [
        VimModule {
            path: Some("plugin/somefile.vim"),
            doc: Some("File header comment"),
            nodes: [ … ],
        },
    ],
    …
}
```
Python:
```python
import vim_plugin_metadata

vim_plugin_metadata.VimParser().parse_plugin_dir(".vim/plugged/someplugin")
```
```
VimPlugin([VimModule("plugin/somefile.vim", doc="File header comment", nodes=[…]), …])
```
