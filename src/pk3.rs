//! Resolution of GSC scripts shipped inside `.pk3` archives.
//!
//! A `.pk3` file is a zip archive. When a script references another script
//! (e.g. `maps\mp\_utility::foo`) that is not part of the workspace, the
//! `.pk3` files in each directory listed in the `game_paths` field of the
//! `.brenz` config are searched for it, case-insensitively.
//!
//! The archive listing (which `.pk3` holds which script) is cached in
//! `<workspace>/.cache/brenz/pk3_index.ron`, clangd style, and refreshed
//! from the per-archive size plus modification time: only new, removed or
//! changed archives are re-scanned.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::util::{LogType, logprint};

/// Where the index cache lives, relative to the workspace root.
const CACHE_DIR_NAME: &str = ".cache";
const CACHE_SUBDIR_NAME: &str = "brenz";
const CACHE_FILE_NAME: &str = "pk3_index.ron";
/// Previous cache file name, removed once the RON cache is written.
const OLD_CACHE_FILE_NAME: &str = "pk3_index.toml";

/// One script found inside one archive.
#[derive(Debug, Clone)]
pub(crate) struct Pk3Entry {
    /// Absolute path of the `.pk3` archive.
    pub pk3_path: PathBuf,
    /// Path of the script inside the archive, in original casing.
    pub inner_path: String,
    /// Position of the archive's directory in `game_paths`.
    /// Files from later defined game paths take precedence.
    pub(crate) dir_order: usize,
}

impl Pk3Entry {
    /// Precedence rank, highest wins: later game path first, then
    /// later archive file name (so `pak1.pk3` overrides `pak0.pk3`).
    fn rank(&self) -> (usize, String) {
        let file = self
            .pk3_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        (self.dir_order, file)
    }
}

/// Order hits best-first by precedence rank.
fn best_first(mut hits: Vec<Pk3Entry>) -> Vec<Pk3Entry> {
    hits.sort_by_key(|e| std::cmp::Reverse(e.rank()));
    hits
}

/// Index of script paths (lowercased) to the archives holding them.
#[derive(Debug, Default)]
pub(crate) struct Pk3Index {
    entries: HashMap<String, Vec<Pk3Entry>>,
}

impl Pk3Index {
    /// Normalize an archive member name (or a lookup candidate) so that
    /// lookups are case-insensitive and separator-insensitive. Repeated
    /// separators and leading `./` segments collapse away.
    pub(crate) fn normalize(name: &str) -> String {
        let mut n = name.replace('\\', "/");
        loop {
            let next = n.replace("//", "/");
            if next.len() == n.len() {
                break;
            }
            n = next;
        }
        while let Some(stripped) = n.strip_prefix("./").or_else(|| n.strip_prefix('/')) {
            n = stripped.to_string();
        }
        n.to_lowercase()
    }

    /// Exact-match lookup candidates for a script reference, most
    /// specific first: `<path>/<script>.gsc`, then `<script>.gsc`.
    fn candidates(path: Option<&str>, script: &str) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(p) = path {
            let p = p.replace('\\', "/");
            let p = p.strip_suffix('/').unwrap_or(&p);
            if !p.is_empty() {
                out.push(Self::normalize(&format!("{p}/{script}.gsc")));
            }
        }
        out.push(Self::normalize(&format!("{script}.gsc")));
        out
    }

    /// Find archives holding the referenced script, best match first.
    ///
    /// Tries the exact candidates first. A reference without a path
    /// (`script::func`) additionally falls back to any script whose path
    /// ends with `/<script>.gsc`, since such references resolve relative
    /// to the calling file. Results are sorted for determinism.
    ///
    /// Precedence: files from later defined game paths are returned
    /// first, then later archive file names within one path.
    pub(crate) fn lookup(&self, path: Option<&str>, script: &str) -> Vec<Pk3Entry> {
        for key in Self::candidates(path, script) {
            if let Some(hits) = self.entries.get(&key) {
                return best_first(hits.clone());
            }
        }
        let suffix = format!("/{}", Self::normalize(&format!("{script}.gsc")));
        let mut keys: Vec<&String> = self
            .entries
            .keys()
            .filter(|k| k.ends_with(suffix.as_str()))
            .collect();
        keys.sort();
        best_first(
            keys.into_iter()
                .flat_map(|k| self.entries.get(k).unwrap().clone())
                .collect(),
        )
    }

    /// Read a script's text from its archive.
    ///
    /// The member match is case-insensitive: the index key may have been
    /// built from a differently-cased listing, so members are compared
    /// normalized rather than with a case-sensitive name lookup.
    pub(crate) fn read_script_text(&self, entry: &Pk3Entry) -> io::Result<String> {
        read_member_case_insensitive(&entry.pk3_path, &entry.inner_path)
    }

    pub(crate) fn entry_count(&self) -> usize {
        self.entries.values().map(Vec::len).sum()
    }

    /// Build the index for `pk3_paths`, reusing the on-disk cache in
    /// `<workspace_root>/.cache/brenz/` where the archives are unchanged.
    /// With no workspace root the index is built in memory only.
    /// Relative game paths resolve against the workspace root, so the
    /// server's working directory never matters.
    pub(crate) fn load_or_build(workspace_root: Option<&PathBuf>, pk3_paths: &[PathBuf]) -> Self {
        let pk3_paths: Vec<PathBuf> = pk3_paths
            .iter()
            .map(|p| {
                if p.is_absolute() {
                    p.clone()
                } else {
                    workspace_root
                        .map(|r| r.join(p))
                        .unwrap_or_else(|| p.clone())
                }
            })
            .collect();
        let pk3_paths = &pk3_paths;
        let archives = list_pk3_archives(pk3_paths);
        if archives.is_empty() {
            return Self::default();
        }

        let cache_path = workspace_root.map(|r| {
            r.join(CACHE_DIR_NAME)
                .join(CACHE_SUBDIR_NAME)
                .join(CACHE_FILE_NAME)
        });

        let mut cached = cache_path.as_ref().and_then(|p| read_cache(p, pk3_paths));

        let mut index = Self::default();
        let mut fresh: Vec<CachedArchive> = Vec::new();

        for (dir_order, pk3) in &archives {
            let dir_order = *dir_order;
            let meta = match archive_fingerprint(pk3) {
                Some(m) => m,
                None => continue,
            };
            if let Some(reused) = cached.as_mut().and_then(|c| c.take_matching(&meta)) {
                let order = dir_order_of(pk3_paths, &meta.path);
                for e in &reused.entries {
                    index.insert(
                        &e.key,
                        Pk3Entry {
                            pk3_path: meta.path.clone(),
                            inner_path: e.inner.clone(),
                            dir_order: order,
                        },
                    );
                }
                fresh.push(reused);
                continue;
            }
            match scan_archive(pk3) {
                Ok(scanned) => {
                    for (key, inner) in &scanned {
                        index.insert(
                            key,
                            Pk3Entry {
                                pk3_path: pk3.clone(),
                                inner_path: inner.clone(),
                                dir_order,
                            },
                        );
                    }
                    fresh.push(CachedArchive {
                        path: meta.path.clone(),
                        mtime_secs: meta.mtime_secs,
                        size: meta.size,
                        entries: scanned
                            .into_iter()
                            .map(|(key, inner)| CachedEntry { key, inner })
                            .collect(),
                    });
                }
                Err(e) => {
                    logprint!(
                        LogType::Error,
                        "Skipping unreadable archive {}: {e}",
                        pk3.display()
                    );
                }
            }
        }

        if let Some(path) = cache_path {
            let file = Pk3CacheFile {
                pk3_paths: pk3_paths.to_vec(),
                archives: fresh,
            };
            if let Err(e) = write_cache(&path, &file) {
                logprint!(
                    LogType::Error,
                    "Couldn't write pk3 index cache {}: {e}",
                    path.display()
                );
            }
        }

        logprint!(
            LogType::Info,
            "Pk3 index: {} scripts in {} archives",
            index.entry_count(),
            archives.len()
        );
        index
    }

    fn insert(&mut self, key: &str, entry: Pk3Entry) {
        self.entries.entry(key.to_string()).or_default().push(entry);
    }
}

/// Case-insensitive read of a single member of a zip archive.
fn read_member_case_insensitive(pk3_path: &Path, inner_path: &str) -> io::Result<String> {
    let wanted = Pk3Index::normalize(inner_path);
    let file = File::open(pk3_path)?;
    let mut zip =
        zip::ZipArchive::new(file).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    for i in 0..zip.len() {
        let mut f = zip
            .by_index(i)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if f.is_dir() {
            continue;
        }
        // `name()` borrows `f`, so copy out before reading contents.
        let name = f.name().to_string();
        if Pk3Index::normalize(&name) == wanted {
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)?;
            return Ok(String::from_utf8_lossy(&buf).into_owned());
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("{inner_path} not found in {}", pk3_path.display()),
    ))
}

/// Size plus modification time identifying an unchanged archive.
struct ArchiveFingerprint {
    path: PathBuf,
    mtime_secs: i64,
    size: u64,
}

fn archive_fingerprint(pk3: &Path) -> Option<ArchiveFingerprint> {
    let meta = std::fs::metadata(pk3).ok()?;
    let mtime_secs = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    Some(ArchiveFingerprint {
        path: pk3.to_path_buf(),
        mtime_secs,
        size: meta.len(),
    })
}

/// Position of an archive's directory in `game_paths`. Unknown
/// directories sort last rather than failing the lookup. Comparison
/// canonicalizes both sides so trailing slashes, `.` segments and
/// symlinks do not break the match.
fn dir_order_of(game_paths: &[PathBuf], archive: &Path) -> usize {
    let parent = archive.parent();
    game_paths
        .iter()
        .position(|d| same_dir(d, parent))
        .unwrap_or(usize::MAX)
}

fn same_dir(dir: &Path, parent: Option<&Path>) -> bool {
    let Some(parent) = parent else {
        return false;
    };
    if dir == parent {
        return true;
    }
    match (std::fs::canonicalize(dir), std::fs::canonicalize(parent)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// `.pk3` files (case-insensitive extension) directly inside `game_paths`,
/// as `(position of the directory in game_paths, archive)` pairs sorted by
/// path for determinism. Missing directories are skipped with a warning.
fn list_pk3_archives(game_paths: &[PathBuf]) -> Vec<(usize, PathBuf)> {
    let mut out = Vec::new();
    for (dir_order, dir) in game_paths.iter().enumerate() {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                logprint!(LogType::Error, "Skipping game path {}: {e}", dir.display());
                continue;
            }
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pk3")) {
                out.push((dir_order, p));
            }
        }
    }
    out.sort();
    out
}

/// Read one archive's central directory without touching file
/// contents: names and cases only, which is all the index needs.
fn scan_archive(pk3: &Path) -> io::Result<Vec<(String, String)>> {
    let file = File::open(pk3)?;
    let mut zip =
        zip::ZipArchive::new(file).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let mut out = Vec::new();
    for i in 0..zip.len() {
        let f = zip
            .by_index(i)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if f.is_dir() {
            continue;
        }
        let inner = f.name().to_string();
        if inner.to_lowercase().ends_with(".gsc") {
            out.push((Pk3Index::normalize(&inner), inner));
        }
    }
    out.sort();
    Ok(out)
}

#[derive(Debug, Serialize, Deserialize)]
struct Pk3CacheFile {
    #[serde(default)]
    pk3_paths: Vec<PathBuf>,
    #[serde(default)]
    archives: Vec<CachedArchive>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedArchive {
    path: PathBuf,
    mtime_secs: i64,
    size: u64,
    #[serde(default)]
    entries: Vec<CachedEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedEntry {
    key: String,
    inner: String,
}

/// Leftover cached archives that have not been claimed by `take_matching`.
struct PendingCache {
    archives: HashMap<PathBuf, CachedArchive>,
}

impl PendingCache {
    fn take_matching(&mut self, meta: &ArchiveFingerprint) -> Option<CachedArchive> {
        let hit = self.archives.get(&meta.path)?;
        if hit.mtime_secs == meta.mtime_secs && hit.size == meta.size {
            self.archives.remove(&meta.path)
        } else {
            None
        }
    }
}

/// Read the on-disk cache, or bail (`None`) when anything looks
/// off: missing file, bad parse, or different game paths. A missed
/// cache just means rescanning, never an error.
fn read_cache(path: &Path, pk3_paths: &[PathBuf]) -> Option<PendingCache> {
    let content = std::fs::read_to_string(path).ok()?;
    let file: Pk3CacheFile = ron::from_str(&content).ok()?;
    if file.pk3_paths != pk3_paths {
        return None;
    }
    Some(PendingCache {
        archives: file
            .archives
            .into_iter()
            .map(|a| (a.path.clone(), a))
            .collect(),
    })
}

/// Write the fresh cache and sweep the old TOML file away so stale
/// formats never linger beside the new one.
fn write_cache(path: &Path, file: &Pk3CacheFile) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content =
        ron::ser::to_string_pretty(file, ron::ser::PrettyConfig::new().struct_names(true))
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, content)?;
    // Drop the artifact of the old TOML cache, if present.
    let _ = std::fs::remove_file(path.with_file_name(OLD_CACHE_FILE_NAME));
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_helpers {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A fresh empty directory under the system temp dir.
    pub(crate) fn fresh_temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "brenz_test_{}_{}_{}",
            tag,
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use test_helpers::fresh_temp_dir;

    fn make_pk3(dir: &Path, name: &str, members: &[(&str, &str)]) -> PathBuf {
        let path = dir.join(name);
        let file = File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        for (inner, content) in members {
            zip.start_file(
                *inner,
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    #[test]
    fn normalize_is_case_and_separator_insensitive() {
        assert_eq!(
            Pk3Index::normalize("maps\\MP\\_utility.GSC"),
            "maps/mp/_utility.gsc"
        );
        assert_eq!(Pk3Index::normalize("/maps/dm.gsc"), "maps/dm.gsc");
        assert_eq!(Pk3Index::normalize("./maps/dm.gsc"), "maps/dm.gsc");
        assert_eq!(Pk3Index::normalize("././maps//dm.gsc"), "maps/dm.gsc");
    }

    #[test]
    fn mixed_case_extension_is_indexed() {
        let dir = fresh_temp_dir("pk3mixedext");
        make_pk3(&dir, "pak0.pk3", &[("maps/mp/dm.Gsc", "main() {}\n")]);
        let index = Pk3Index::load_or_build(None, &[dir]);
        assert_eq!(index.entry_count(), 1);
        assert_eq!(index.lookup(Some("maps/mp/"), "dm").len(), 1);
    }

    #[test]
    fn lookup_is_case_insensitive() {
        let dir = fresh_temp_dir("pk3lookup");
        make_pk3(
            &dir,
            "pak0.PK3",
            &[("Maps\\MP\\_utility.gsc", "main() {}\n")],
        );
        let index = Pk3Index::load_or_build(None, &[dir]);
        let hits = index.lookup(Some("maps\\mp\\"), "_UTILITY");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].inner_path, "Maps\\MP\\_utility.gsc");
        let text = index.read_script_text(&hits[0]).unwrap();
        assert_eq!(text, "main() {}\n");
    }

    #[test]
    fn bare_script_falls_back_to_suffix_match() {
        let dir = fresh_temp_dir("pk3suffix");
        make_pk3(&dir, "pak0.pk3", &[("maps/mp/dm.gsc", "main() {}\n")]);
        let index = Pk3Index::load_or_build(None, &[dir]);
        let hits = index.lookup(None, "dm");
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn later_game_path_wins() {
        let first = fresh_temp_dir("pk3prec_first");
        let second = fresh_temp_dir("pk3prec_second");
        make_pk3(&first, "pak0.pk3", &[("maps/mp/dm.gsc", "old() {}\n")]);
        make_pk3(&second, "pak0.pk3", &[("maps/mp/dm.gsc", "new() {}\n")]);
        let index = Pk3Index::load_or_build(None, &[first, second.clone()]);
        let hits = index.lookup(Some("maps/mp/"), "dm");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].pk3_path.parent().unwrap(), second);
        let text = index.read_script_text(&hits[0]).unwrap();
        assert_eq!(text, "new() {}\n");
    }

    #[test]
    fn later_archive_wins_within_one_path() {
        let dir = fresh_temp_dir("pk3prec_arc");
        make_pk3(&dir, "pak0.pk3", &[("maps/mp/dm.gsc", "old() {}\n")]);
        make_pk3(&dir, "pak1.pk3", &[("maps/mp/dm.gsc", "new() {}\n")]);
        let index = Pk3Index::load_or_build(None, &[dir]);
        let hits = index.lookup(Some("maps/mp/"), "dm");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].pk3_path.file_name().unwrap(), "pak1.pk3");
    }

    #[test]
    fn missing_script_gives_no_hits() {
        let dir = fresh_temp_dir("pk3miss");
        make_pk3(&dir, "pak0.pk3", &[("maps/mp/dm.gsc", "main() {}\n")]);
        let index = Pk3Index::load_or_build(None, &[dir]);
        assert!(index.lookup(Some("maps/mp/"), "nosuchscript").is_empty());
    }

    #[test]
    fn cache_roundtrip_reuses_entries() {
        let root = fresh_temp_dir("pk3cache_ws");
        let games = fresh_temp_dir("pk3cache_games");
        make_pk3(&games, "pak0.pk3", &[("maps/mp/dm.gsc", "main() {}\n")]);
        let first = Pk3Index::load_or_build(Some(&root), std::slice::from_ref(&games));
        assert_eq!(first.entry_count(), 1);
        let cache_file = root.join(".cache/brenz/pk3_index.ron");
        assert!(cache_file.exists());
        let second = Pk3Index::load_or_build(Some(&root), &[games]);
        assert_eq!(second.entry_count(), 1);
        assert_eq!(second.lookup(None, "dm").len(), 1);
    }

    #[test]
    fn changed_game_paths_invalidate_cache() {
        let root = fresh_temp_dir("pk3cache_inv_ws");
        let games_a = fresh_temp_dir("pk3cache_inv_a");
        let games_b = fresh_temp_dir("pk3cache_inv_b");
        make_pk3(&games_a, "a.pk3", &[("maps/a.gsc", "main() {}\n")]);
        make_pk3(&games_b, "b.pk3", &[("maps/b.gsc", "main() {}\n")]);
        let first = Pk3Index::load_or_build(Some(&root), &[games_a]);
        assert_eq!(first.entry_count(), 1);
        let second = Pk3Index::load_or_build(Some(&root), &[games_b]);
        assert_eq!(second.entry_count(), 1);
        assert!(second.lookup(None, "a").is_empty());
        assert_eq!(second.lookup(None, "b").len(), 1);
    }
}
