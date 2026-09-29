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
The `.brenz` file in your project root is a TOML config, like `.clangd`
for clangd. It currently holds one field:

```toml
# Game installation directories to resolve scripts from `.pk3` archives.
game_paths = ["/home/user/games/cod"]
```

When a script references another script (e.g. `maps\mp\_utility::foo`)
that is not in the workspace, Brenz scans the `.pk3` files in each
`game_paths` directory for it. Matching inside archives is
case-insensitive. If several archives hold the same script, files from
later defined game paths are used, then later archive file names within
one path (so `pak1.pk3` overrides `pak0.pk3`). A found script is parsed
too, so its own dependencies resolve as well. Go-to-definition on such scripts points at
`pk3://...` locations backed by the archive contents.

The archive listing is cached in `.cache/brenz/pk3_index.toml` inside the
project (clangd style) and refreshed automatically: only new, removed or
changed archives are re-scanned.
