//! Project configuration stored in the `.brenz` file.
//!
//! The `.brenz` file lives in the workspace root and uses RON syntax,
//! like the builtin docs. It currently holds:
//!
//! ```ron
//! // Brenz project file.
//! // `root` anchors workspace script lookups, e.g. `sv` below, so
//! // `maps\mp\gametypes\tdm` finds `sv/maps/mp/gametypes/tdm.gsc`.
//! // `include_paths` are extra search roots: loose `.gsc` files and
//! // `.pk3` archives in them resolve scripts the workspace lacks.
//! (
//!     root: Some("sv"),
//!     include_paths: ["/home/user/games/cod"],
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
    /// Project script root: workspace lookups for `path\script`
    /// resolve under here. Relative paths resolve against the
    /// workspace root; unset means the workspace root itself.
    #[serde(default)]
    pub root: Option<PathBuf>,
    /// Extra search roots for scripts outside the workspace: loose
    /// `.gsc` files (found recursively) and `.pk3` archives within.
    #[serde(default)]
    pub include_paths: Vec<PathBuf>,
}

impl BrenzConfig {
    /// Template written by `brenz --init`.
    pub(crate) fn template() -> &'static str {
        r#"// Brenz project file (RON configuration).
// `root` anchors workspace script lookups, e.g. `sv` below, so
// `maps\mp\gametypes\tdm` finds `sv/maps/mp/gametypes/tdm.gsc`.
// `include_paths` are extra search roots holding loose `.gsc` files
// and `.pk3` archives, e.g.:
// include_paths: ["/home/user/games/cod/main", "/home/user/games/cod/uo"],
(
    root: None,
    include_paths: [],
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
                if content.contains("pk3_paths =") || content.contains("game_paths =") {
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
        let cfg: BrenzConfig = ron::from_str(BrenzConfig::template()).unwrap();
        assert!(cfg.include_paths.is_empty());
        assert!(cfg.root.is_none());
    }

    #[test]
    fn parses_root_and_include_paths() {
        let cfg: BrenzConfig =
            ron::from_str("(root: Some(\"sv\"), include_paths: [\"/games/cod\"])").unwrap();
        assert_eq!(cfg.root, Some(PathBuf::from("sv")));
        assert_eq!(cfg.include_paths, vec![PathBuf::from("/games/cod")]);
    }

    #[test]
    fn missing_root_defaults_to_none() {
        let cfg: BrenzConfig = ron::from_str("(include_paths: [])").unwrap();
        assert!(cfg.root.is_none());
    }

    #[test]
    fn ignores_unknown_fields() {
        let cfg: BrenzConfig = ron::from_str("(include_paths: [], future_option: 42)").unwrap();
        assert!(cfg.include_paths.is_empty());
    }

    #[test]
    fn missing_file_gives_default() {
        let cfg = BrenzConfig::load(Some(&PathBuf::from("/definitely/not/a/real/dir")));
        assert!(cfg.include_paths.is_empty());
    }

    #[test]
    fn broken_file_gives_default() {
        let dir = crate::include::test_helpers::fresh_temp_dir("brenz_cfg");
        std::fs::write(dir.join(CONFIG_FILE_NAME), "(include_paths: [oops\n").unwrap();
        let cfg = BrenzConfig::load(Some(&dir));
        assert!(cfg.include_paths.is_empty());
    }

    #[test]
    fn old_toml_file_gives_default() {
        let dir = crate::include::test_helpers::fresh_temp_dir("brenz_cfg_toml");
        std::fs::write(dir.join(CONFIG_FILE_NAME), "pk3_paths = []\n").unwrap();
        let cfg = BrenzConfig::load(Some(&dir));
        assert!(cfg.include_paths.is_empty());
    }
}
