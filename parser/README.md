# tree-sitter-gsc (vendored)

GSC grammar for Call of Duty (2003) scripts, vendored from
https://gitlab.com/kazam0180/cod1-gsc-parser at commit `d2c5d59`
("more field names") so Brenz builds from this repository alone.

To update: copy `bindings/rust/*`, `grammar.js`, `src/parser.c`,
`src/grammar.json`, `src/node-types.json` and `src/tree_sitter/*` from
upstream, then refresh the commit hash above. `LICENSE` is the upstream
GPLv3 text, same license as Brenz itself.
