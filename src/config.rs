//! Project configuration stored in the `.brenz` file.
//!
//! The `.brenz` file lives in the workspace root and uses TOML syntax,
//! similar to how `.clangd` configures clangd. Currently it holds:
//!
//! ```toml
//! # Brenz project file.
//! # Point Brenz at your game installation(s) so scripts shipped inside
//! # `.pk3` archives can be resolved, e.g.:
//! # game_paths = ["C:/Program Files/Call of Duty", "/home/user/games/cod"]
//! game_paths = []
//! ```
//!
//! Unknown fields are ignored so newer Brenz versions can extend the file
//! without breaking older ones.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::util::{LogType, logprint};

/// Name of the project file, relative to the workspace root.
pub(crate) const CONFIG_FILE_NAME: &str = ".brenz";

/// Parsed contents of the `.brenz` project file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct BrenzConfig {
    /// Game installation directories to search for `.pk3` archives.
    #[serde(default)]
    pub game_paths: Vec<PathBuf>,
}

impl BrenzConfig {
    /// Template written by `brenz --init`.
    pub(crate) fn template() -> &'static str {
        r#"# Brenz project file (TOML configuration).
# Point Brenz at your game installation(s) so scripts shipped inside
# `.pk3` archives can be resolved, e.g.:
# game_paths = ["C:/Program Files/Call of Duty", "/home/user/games/cod"]
game_paths = []
"#
    }

    /// Load the config from `<workspace_root>/.brenz`.
    ///
    /// A missing file (or no workspace root at all) yields an empty config.
    /// A file that fails to parse also yields an empty config, with the
    /// error logged, so a broken config never kills the language server.
    pub(crate) fn load(workspace_root: Option<&PathBuf>) -> Self {
        let Some(root) = workspace_root else {
            return Self::default();
        };
        let path = root.join(CONFIG_FILE_NAME);
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                logprint!(
                    LogType::Error,
                    "Couldn't read {}: {e}",
                    path.display()
                );
                return Self::default();
            }
        };
        match toml::from_str::<BrenzConfig>(&content) {
            Ok(cfg) => cfg,
            Err(e) => {
                logprint!(
                    LogType::Error,
                    "Couldn't parse {}: {e}",
                    path.display()
                );
                Self::default()
            }
        }
    }

    /// Write a fresh `.brenz` file into `dir`.
    pub(crate) fn init_in(dir: &Path) -> io::Result<PathBuf> {
        let f = dir.join(CONFIG_FILE_NAME);
        if f.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                ".brenz file already exists",
            ));
        }
        std::fs::write(&f, Self::template())?;
        Ok(dir.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses_to_empty_config() {
        let cfg: BrenzConfig = toml::from_str(BrenzConfig::template()).unwrap();
        assert!(cfg.game_paths.is_empty());
    }

    #[test]
    fn parses_game_paths() {
        let cfg: BrenzConfig =
            toml::from_str("game_paths = [\"/games/cod\", \"C:/CoD\"]").unwrap();
        assert_eq!(
            cfg.game_paths,
            vec![
                PathBuf::from("/games/cod"),
                PathBuf::from("C:/CoD")
            ]
        );
    }

    #[test]
    fn ignores_unknown_fields() {
        let cfg: BrenzConfig =
            toml::from_str("game_paths = []\nfuture_option = 42").unwrap();
        assert!(cfg.game_paths.is_empty());
    }

    #[test]
    fn missing_file_gives_default() {
        let cfg = BrenzConfig::load(Some(&PathBuf::from("/definitely/not/a/real/dir")));
        assert!(cfg.game_paths.is_empty());
    }

    #[test]
    fn broken_file_gives_default() {
        let dir = crate::pk3::test_helpers::fresh_temp_dir("brenz_cfg");
        std::fs::write(dir.join(CONFIG_FILE_NAME), "game_paths = [oops\n").unwrap();
        let cfg = BrenzConfig::load(Some(&dir));
        assert!(cfg.game_paths.is_empty());
    }
}
