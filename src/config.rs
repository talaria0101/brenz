//! Project configuration stored in the `.brenz` file.
//!
//! The `.brenz` file lives in the workspace root and uses RON syntax,
//! like the builtin docs. It currently holds:
//!
//! ```ron
//! // Brenz project file.
//! // Point Brenz at your game installation(s) so scripts shipped inside
//! // `.pk3` archives can be resolved, e.g.:
//! // pk3_paths: ["/home/user/games/cod/main", "/home/user/games/cod/uo"],
//! (
//!     pk3_paths: [],
//! )
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
    pub pk3_paths: Vec<PathBuf>,
}

impl BrenzConfig {
    /// Template written by `brenz --init`.
    pub(crate) fn template() -> &'static str {
        r#"// Brenz project file (RON configuration).
// Point Brenz at your game installation(s) so scripts shipped inside
// `.pk3` archives can be resolved, e.g.:
// pk3_paths: ["/home/user/games/cod/main", "/home/user/games/cod/uo"],
(
    pk3_paths: [],
)
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
                logprint!(LogType::Error, "Couldn't read {}: {e}", path.display());
                return Self::default();
            }
        };
        match ron::from_str::<BrenzConfig>(&content) {
            Ok(cfg) => cfg,
            Err(e) => {
                if content.contains("game_paths =") {
                    logprint!(
                        LogType::Error,
                        "{} is in the old TOML format, rewrite it as RON (see `brenz --init`): {e}",
                        path.display()
                    );
                } else {
                    logprint!(LogType::Error, "Couldn't parse {}: {e}", path.display());
                }
                Self::default()
            }
        }
    }

    /// Write a fresh `.brenz` file into `dir`. Refuses when one
    /// already exists rather than clobbering someone's config.
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
        let cfg: BrenzConfig = ron::from_str(BrenzConfig::template()).unwrap();
        assert!(cfg.pk3_paths.is_empty());
    }

    #[test]
    fn parses_game_paths() {
        let cfg: BrenzConfig = ron::from_str("(pk3_paths: [\"/games/cod/main\", \"/games/cod/uo\"])").unwrap();
        assert_eq!(
            cfg.pk3_paths,
            vec![PathBuf::from("/games/cod"), PathBuf::from("/games/cod/uo")]
        );
    }

    #[test]
    fn ignores_unknown_fields() {
        let cfg: BrenzConfig = ron::from_str("(game_paths: [], future_option: 42)").unwrap();
        assert!(cfg.pk3_paths.is_empty());
    }

    #[test]
    fn missing_file_gives_default() {
        let cfg = BrenzConfig::load(Some(&PathBuf::from("/definitely/not/a/real/dir")));
        assert!(cfg.pk3_paths.is_empty());
    }

    #[test]
    fn broken_file_gives_default() {
        let dir = crate::pk3::test_helpers::fresh_temp_dir("brenz_cfg");
        std::fs::write(dir.join(CONFIG_FILE_NAME), "(pk3_paths: [oops\n").unwrap();
        let cfg = BrenzConfig::load(Some(&dir));
        assert!(cfg.pk3_paths.is_empty());
    }

    #[test]
    fn old_toml_file_gives_default() {
        let dir = crate::pk3::test_helpers::fresh_temp_dir("brenz_cfg_toml");
        std::fs::write(dir.join(CONFIG_FILE_NAME), "pk3_paths = []\n").unwrap();
        let cfg = BrenzConfig::load(Some(&dir));
        assert!(cfg.pk3_paths.is_empty());
    }
}
