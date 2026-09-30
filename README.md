###### toplink

# Brenz
Language server for Call of Duty 1/United Offensive GSC.

Only the parser works currently and is in testing stage. Feel free to try from the `bin` directory.

Instructions for setting up for Kate editor can be found [here](https://gitlab.com/kazam0180/kate-gsc).

## Usage
Run Brenz with `--init` argument in your project directory before working. E.g.
```bash
cd project
brenz --init
```
Run Brenz with `--backtrace` argument to get a backtrace for errors.

## Configuration
The `.brenz` file in your project root is a RON config. It currently
holds two fields:

```ron
// `root` anchors workspace script lookups, e.g. `sv` below, so
// `maps\mp\gametypes\tdm` finds `sv/maps/mp/gametypes/tdm.gsc`.
// `include_paths` are extra search roots holding loose `.gsc`
// files and `.pk3` archives, e.g.:
// include_paths: ["/home/user/games/cod/main", "/home/user/games/cod/uo"],
(
    root: Some("sv"),
    include_paths: [],
)
```

When a script references another script (e.g. `maps\mp\_utility::foo`)
that is not under `root`, Brenz searches each `include_paths`
directory for it: loose `.gsc` files (found recursively) and the
`.pk3` files inside (top level only). Matching is case-insensitive.
Workspace files always win over includes; among includes, later
defined paths win, then archives over loose files within one path
(the game loads packed files first), then later file names. A found
script is parsed too, so its own dependencies resolve as well.
Go-to-definition on archive scripts points at `pk3://...` locations
backed by the archive contents, while loose files point at their
real path.

The archive listing is cached in `.cache/brenz/pk3_index.ron` inside the
project (clangd style) and refreshed automatically: only new, removed or
changed archives are re-scanned.

## Diagnostics
Beyond syntax errors, Brenz reports what the script VM provably
rejects: unknown functions, unknown scripts, functions missing from
the referenced script, builtin arity and argument type mismatches,
`break`/`continue` outside loops, duplicate functions and cases, and
`foreach`, which does not exist in this engine. Anything that depends
on runtime values is left alone. A quickfix rewrites `foreach` loops
into `for` loops over `.size`.
