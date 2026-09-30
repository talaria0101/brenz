//! Resolution of GSC scripts outside the workspace.
//!
//! Each directory in the `include_paths` field of the `.brenz` config
//! is searched two ways: loose `.gsc` files (found recursively) and
//! `.pk3` archives (listed top-level; a `.pk3` is a zip archive).
//! When a script references another script (e.g.
//! `maps\mp\_utility::foo`) that is not in the workspace, both are
//! searched case-insensitively.
//!
//! Precedence, best first: later defined include paths win, then
//! archives over loose files within one path (that is how the game
//! loads them), then later file names. Workspace files always win
//! over everything here; that is decided by the caller.
//!
//! The listing (which include holds which script) is cached in
//! `<workspace>/.cache/brenz/include_index.ron`, clangd style, and
//! refreshed from per-file size plus modification time: only new,
//! removed or changed files are re-scanned.

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
const CACHE_FILE_NAME: &str = "include_index.ron";
/// Previous cache file names, removed once the new cache is written.
const OLD_CACHE_FILES: &[&str] = &["pk3_index.ron", "pk3_index.toml"];

/// Where one indexed script lives.
#[derive(Debug, Clone)]
pub(crate) enum ScriptSource {
    /// Inside an archive: absolute `.pk3` path plus the member path
    /// in original casing.
    Archive { pk3: PathBuf, inner: String },
    /// A loose file: absolute path.
    Loose { path: PathBuf },
}

/// One script found in the include paths.
#[derive(Debug, Clone)]
pub(crate) struct IncludeEntry {
    pub source: ScriptSource,
    /// Position of the include directory in `include_paths`.
    /// Files from later defined paths take precedence.
    pub(crate) dir_order: usize,
}

impl IncludeEntry {
    /// Precedence rank, highest wins: later include path first, then
    /// archives over loose files (the game loads packed files
    /// first), then later file names within one path (so `pak1.pk3`
    /// overrides `pak0.pk3`).
    fn rank(&self) -> (usize, bool, String) {
        let (packed, file) = match &self.source {
            ScriptSource::Archive { pk3, .. } => (true, file_name(pk3)),
            ScriptSource::Loose { path } => (false, file_name(path)),
        };
        (self.dir_order, packed, file)
    }

    /// Display name for logs: archive member or file path.
    pub(crate) fn display(&self) -> String {
        match &self.source {
            ScriptSource::Archive { pk3, inner } => {
                format!("{} in {}", inner, pk3.display())
            }
            ScriptSource::Loose { path } => path.display().to_string(),
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Order hits best-first by precedence rank.
fn best_first(mut hits: Vec<IncludeEntry>) -> Vec<IncludeEntry> {
    hits.sort_by_key(|e| std::cmp::Reverse(e.rank()));
    hits
}

/// Index of script paths (lowercased) to where they were found.
#[derive(Debug, Default)]
pub(crate) struct IncludeIndex {
    entries: HashMap<String, Vec<IncludeEntry>>,
}

impl IncludeIndex {
    /// Normalize a member or file path (or a lookup candidate) so that
    /// lookups are case-insensitive and separator-insensitive.
    /// Repeated separators and leading `./` segments collapse away.
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

    /// Find where the referenced script lives, best match first.
    ///
    /// Tries the exact candidates first. A reference without a path
    /// (`script::func`) additionally falls back to any script whose path
    /// ends with `/<script>.gsc`, since such references resolve relative
    /// to the calling file. Results are sorted for determinism.
    ///
    /// Precedence: files from later defined include paths are returned
    /// first, then archives over loose files, then later file names.
    pub(crate) fn lookup(&self, path: Option<&str>, script: &str) -> Vec<IncludeEntry> {
        for key in Self::candidates(path, script) {
            if let Some(hits) = self.entries.get(&key) {
                return best_first(hits.clone());
            }
        }
        let suffix = format!("/{}.gsc", script.to_lowercase());
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

    /// Read a script's text from its archive or from disk.
    ///
    /// The archive member match is case-insensitive: the index key may
    /// have been built from a differently-cased listing, so members are
    /// compared normalized rather than with a case-sensitive lookup.
    pub(crate) fn read_script_text(&self, entry: &IncludeEntry) -> io::Result<String> {
        // Timed single-script read: archive members reopen the zip
        // every time, so a slow dependency chain shows up here.
        let start = std::time::Instant::now();
        let out = match &entry.source {
            ScriptSource::Archive { pk3, inner } => read_member_case_insensitive(pk3, inner),
            ScriptSource::Loose { path } => {
                let bytes = std::fs::read(path)?;
                Ok(String::from_utf8_lossy(&bytes).into_owned())
            }
        };
        if let Ok(text) = &out {
            logprint!(
                LogType::Info,
                "Brenz timing: read_script_text bytes={} total={}ms src={}",
                text.len(),
                crate::util::timing_ms(start),
                entry.display()
            );
        }
        out
    }

    pub(crate) fn entry_count(&self) -> usize {
        self.entries.values().map(Vec::len).sum()
    }

    /// Build the index for `include_paths`, reusing the on-disk cache
    /// in `<workspace_root>/.cache/brenz/` where files are unchanged.
    /// With no workspace root the index is built in memory only.
    /// Relative include paths resolve against the workspace root, so
    /// the server's working directory never matters.
    pub(crate) fn load_or_build(
        workspace_root: Option<&PathBuf>,
        include_paths: &[PathBuf],
    ) -> Self {
        // Timed index build: listing, cache reuse, fresh scans and
        // cache write each log separately so a slow include path
        // (big game dir, many archives) is attributable.
        let total_start = std::time::Instant::now();
        let include_paths: Vec<PathBuf> = include_paths
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
        let include_paths = &include_paths;

        let t = std::time::Instant::now();
        let listed = list_include_files(include_paths);
        let list_ms = crate::util::timing_ms(t);
        logprint!(
            LogType::Info,
            "Brenz timing: include_index list files={} total={}ms paths={:?}",
            listed.len(),
            list_ms,
            include_paths
        );
        if listed.is_empty() {
            logprint!(
                LogType::Info,
                "Brenz timing: include_index empty total={}ms",
                crate::util::timing_ms(total_start)
            );
            return Self::default();
        }

        let cache_path = workspace_root.map(|r| {
            r.join(CACHE_DIR_NAME)
                .join(CACHE_SUBDIR_NAME)
                .join(CACHE_FILE_NAME)
        });

        let t = std::time::Instant::now();
        let mut cached = cache_path
            .as_ref()
            .and_then(|p| read_cache(p, include_paths));
        let cache_read_ms = crate::util::timing_ms(t);
        let cache_hit_files = cached.as_ref().map(|c| c.files.len()).unwrap_or(0);

        let mut index = Self::default();
        let mut fresh: Vec<CachedFile> = Vec::new();
        let mut reused_count = 0usize;
        let mut scanned_count = 0usize;
        let scan_start = std::time::Instant::now();

        for (dir_order, file) in &listed {
            let meta = match file_fingerprint(&file.path) {
                Some(m) => m,
                None => continue,
            };
            if let Some(reused) = cached.as_mut().and_then(|c| c.take_matching(&meta)) {
                let order = dir_order_of(include_paths, &meta.path);
                for e in &reused.entries {
                    index.insert(
                        &e.key,
                        IncludeEntry {
                            source: reused.source(&meta.path, &e.inner),
                            dir_order: order,
                        },
                    );
                }
                fresh.push(reused);
                reused_count += 1;
                continue;
            }
            let one_start = std::time::Instant::now();
            match scan_file(file, &include_paths[*dir_order]) {
                Ok(scanned) => {
                    let one_ms = crate::util::timing_ms(one_start);
                    if one_ms >= 100 {
                        logprint!(
                            LogType::Info,
                            "Brenz timing: include_index scan file={} scripts={} total={}ms",
                            file.display(),
                            scanned.len(),
                            one_ms
                        );
                    }
                    scanned_count += 1;
                    for (key, inner) in &scanned {
                        index.insert(
                            key,
                            IncludeEntry {
                                source: source_of(file, inner),
                                dir_order: *dir_order,
                            },
                        );
                    }
                    fresh.push(CachedFile {
                        path: meta.path.clone(),
                        mtime_secs: meta.mtime_secs,
                        size: meta.size,
                        loose: matches!(file.kind, FileKind::Loose),
                        entries: scanned
                            .into_iter()
                            .map(|(key, inner)| CachedEntry { key, inner })
                            .collect(),
                    });
                }
                Err(e) => {
                    logprint!(
                        LogType::Error,
                        "Skipping unreadable {}: {e}",
                        file.display()
                    );
                }
            }
        }
        let scan_ms = crate::util::timing_ms(scan_start);

        let t = std::time::Instant::now();
        if let Some(path) = cache_path {
            let file = IncludeCacheFile {
                include_paths: include_paths.to_vec(),
                files: fresh,
            };
            if let Err(e) = write_cache(&path, &file) {
                logprint!(
                    LogType::Error,
                    "Couldn't write include index cache {}: {e}",
                    path.display()
                );
            }
        }
        let cache_write_ms = crate::util::timing_ms(t);

        logprint!(
            LogType::Info,
            "Include index: {} scripts in {} files",
            index.entry_count(),
            listed.len()
        );
        logprint!(
            LogType::Info,
            "Brenz timing: include_index list={}ms cache_read={}ms cached_files={} reused={} scanned={} scan={}ms cache_write={}ms scripts={} total={}ms",
            list_ms,
            cache_read_ms,
            cache_hit_files,
            reused_count,
            scanned_count,
            scan_ms,
            cache_write_ms,
            index.entry_count(),
            crate::util::timing_ms(total_start)
        );
        index
    }

    fn insert(&mut self, key: &str, entry: IncludeEntry) {
        self.entries.entry(key.to_string()).or_default().push(entry);
    }
}

/// A listed file: a top-level `.pk3` archive or a loose `.gsc` file.
struct ListedFile {
    kind: FileKind,
    path: PathBuf,
}

#[derive(Clone, Copy, PartialEq)]
enum FileKind {
    Archive,
    Loose,
}

impl ListedFile {
    fn display(&self) -> String {
        self.path.display().to_string()
    }
}

/// Build a source value for a fresh scan hit.
fn source_of(file: &ListedFile, inner: &str) -> ScriptSource {
    match file.kind {
        FileKind::Archive => ScriptSource::Archive {
            pk3: file.path.clone(),
            inner: inner.to_string(),
        },
        FileKind::Loose => ScriptSource::Loose {
            path: file.path.clone(),
        },
    }
}

/// Case-insensitive read of a single member of a zip archive.
fn read_member_case_insensitive(pk3_path: &Path, inner_path: &str) -> io::Result<String> {
    let wanted = IncludeIndex::normalize(inner_path);
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
        if IncludeIndex::normalize(&name) == wanted {
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

/// Size plus modification time identifying an unchanged file.
struct FileFingerprint {
    path: PathBuf,
    mtime_secs: i64,
    size: u64,
}

fn file_fingerprint(path: &Path) -> Option<FileFingerprint> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime_secs = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    Some(FileFingerprint {
        path: path.to_path_buf(),
        mtime_secs,
        size: meta.len(),
    })
}

fn is_gsc_file(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("gsc"))
}

/// `.pk3` files (case-insensitive extension) directly inside each
/// include path, plus loose `.gsc` files found recursively, as
/// `(position of the directory in include_paths, file)` pairs sorted
/// for determinism. Missing directories are skipped with a warning.
/// Hidden directories (`.git` and friends) are never descended into.
fn list_include_files(include_paths: &[PathBuf]) -> Vec<(usize, ListedFile)> {
    let mut out = Vec::new();
    for (dir_order, dir) in include_paths.iter().enumerate() {
        let top = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                logprint!(
                    LogType::Error,
                    "Skipping include path {}: {e}",
                    dir.display()
                );
                continue;
            }
        };
        for entry in top.flatten() {
            let p = entry.path();
            if p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pk3")) {
                out.push((
                    dir_order,
                    ListedFile {
                        kind: FileKind::Archive,
                        path: p,
                    },
                ));
            } else if p.is_dir() {
                walk_gsc(dir_order, &p, &mut out);
            } else if is_gsc_file(&p) {
                out.push((
                    dir_order,
                    ListedFile {
                        kind: FileKind::Loose,
                        path: p,
                    },
                ));
            }
        }
    }
    out.sort_by(|a, b| a.1.path.cmp(&b.1.path));
    out
}

/// Recursive half of [`list_include_files`].
fn walk_gsc(dir_order: usize, dir: &Path, out: &mut Vec<(usize, ListedFile)>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with('.'))
            {
                continue;
            }
            walk_gsc(dir_order, &p, out);
        } else if is_gsc_file(&p) {
            out.push((
                dir_order,
                ListedFile {
                    kind: FileKind::Loose,
                    path: p,
                },
            ));
        }
    }
}

/// Central-directory listing of one archive, or the single key of a
/// loose file: `(key, inner_path)` pairs. Loose files key by their
/// path relative to the include directory, mirroring archive member
/// paths; keys are normalized, inner paths keep their shape.
fn scan_file(file: &ListedFile, include_dir: &Path) -> io::Result<Vec<(String, String)>> {
    match file.kind {
        FileKind::Archive => scan_archive(&file.path),
        FileKind::Loose => {
            let rel = file
                .path
                .strip_prefix(include_dir)
                .unwrap_or(&file.path)
                .to_string_lossy()
                .replace('\\', "/");
            Ok(vec![(IncludeIndex::normalize(&rel), rel)])
        }
    }
}

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
            out.push((IncludeIndex::normalize(&inner), inner));
        }
    }
    out.sort();
    Ok(out)
}

/// Position of a file's include directory in `include_paths`: the
/// deepest directory containing it, so nested loose files attribute
/// to the most specific path. Unknown locations sort last rather
/// than failing the lookup. Comparison canonicalizes both sides so
/// trailing slashes, `.` segments and symlinks do not break it.
fn dir_order_of(include_paths: &[PathBuf], path: &Path) -> usize {
    let mut best: Option<(usize, usize)> = None;
    for (i, dir) in include_paths.iter().enumerate() {
        if same_dir(dir, path.parent()) {
            return i;
        }
        if path.starts_with(dir) {
            let len = dir.as_os_str().len();
            if best.is_none_or(|(_, l)| len > l) {
                best = Some((i, len));
            }
        }
    }
    best.map(|(i, _)| i).unwrap_or(usize::MAX)
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

#[derive(Debug, Serialize, Deserialize)]
struct IncludeCacheFile {
    #[serde(default)]
    include_paths: Vec<PathBuf>,
    #[serde(default)]
    files: Vec<CachedFile>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedFile {
    path: PathBuf,
    mtime_secs: i64,
    size: u64,
    #[serde(default)]
    loose: bool,
    #[serde(default)]
    entries: Vec<CachedEntry>,
}

impl CachedFile {
    /// Rebuild the entry source for a cache hit.
    fn source(&self, path: &Path, inner: &str) -> ScriptSource {
        if self.loose {
            ScriptSource::Loose {
                path: path.to_path_buf(),
            }
        } else {
            ScriptSource::Archive {
                pk3: path.to_path_buf(),
                inner: inner.to_string(),
            }
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedEntry {
    key: String,
    inner: String,
}

/// Leftover cached files that have not been claimed by `take_matching`.
struct PendingCache {
    files: HashMap<PathBuf, CachedFile>,
}

impl PendingCache {
    fn take_matching(&mut self, meta: &FileFingerprint) -> Option<CachedFile> {
        let hit = self.files.get(&meta.path)?;
        if hit.mtime_secs == meta.mtime_secs && hit.size == meta.size {
            self.files.remove(&meta.path)
        } else {
            None
        }
    }
}

fn read_cache(path: &Path, include_paths: &[PathBuf]) -> Option<PendingCache> {
    let content = std::fs::read_to_string(path).ok()?;
    let file: IncludeCacheFile = ron::from_str(&content).ok()?;
    if file.include_paths != include_paths {
        return None;
    }
    Some(PendingCache {
        files: file
            .files
            .into_iter()
            .map(|a| (a.path.clone(), a))
            .collect(),
    })
}

fn write_cache(path: &Path, file: &IncludeCacheFile) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = ron::ser::to_string_pretty(file, crate::util::ron_pcfg().to_owned())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, content)?;
    // Drop the artifacts of the older cache formats, if present.
    for old in OLD_CACHE_FILES {
        let _ = std::fs::remove_file(path.with_file_name(old));
    }
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

    fn make_loose(dir: &Path, rel: &str, content: &str) -> PathBuf {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn normalize_is_case_and_separator_insensitive() {
        assert_eq!(
            IncludeIndex::normalize("maps\\MP\\_utility.GSC"),
            "maps/mp/_utility.gsc"
        );
        assert_eq!(IncludeIndex::normalize("/maps/dm.gsc"), "maps/dm.gsc");
        assert_eq!(IncludeIndex::normalize("./maps/dm.gsc"), "maps/dm.gsc");
        assert_eq!(IncludeIndex::normalize("././maps//dm.gsc"), "maps/dm.gsc");
    }

    #[test]
    fn lookup_is_case_insensitive() {
        let dir = fresh_temp_dir("pk3lookup");
        make_pk3(
            &dir,
            "pak0.PK3",
            &[("Maps\\MP\\_utility.gsc", "main() {}\n")],
        );
        let index = IncludeIndex::load_or_build(None, &[dir]);
        let hits = index.lookup(Some("maps\\mp\\"), "_UTILITY");
        assert_eq!(hits.len(), 1);
        let text = index.read_script_text(&hits[0]).unwrap();
        assert_eq!(text, "main() {}\n");
    }

    #[test]
    fn loose_files_are_found_recursively() {
        let dir = fresh_temp_dir("loosefind");
        make_loose(&dir, "maps/mp/_utility.GSC", "main() {}\n");
        let index = IncludeIndex::load_or_build(None, &[dir]);
        assert_eq!(index.entry_count(), 1);
        let hits = index.lookup(Some("maps/mp/"), "_utility");
        assert_eq!(hits.len(), 1);
        let text = index.read_script_text(&hits[0]).unwrap();
        assert_eq!(text, "main() {}\n");
    }

    #[test]
    fn archives_beat_loose_files_in_one_path() {
        let dir = fresh_temp_dir("looseprec");
        make_pk3(&dir, "pak0.pk3", &[("maps/mp/dm.gsc", "old() {}\n")]);
        make_loose(&dir, "maps/mp/dm.gsc", "new() {}\n");
        let index = IncludeIndex::load_or_build(None, &[dir]);
        let hits = index.lookup(Some("maps/mp/"), "dm");
        assert_eq!(hits.len(), 2);
        let text = index.read_script_text(&hits[0]).unwrap();
        assert_eq!(text, "old() {}\n");
    }

    #[test]
    fn bare_script_falls_back_to_suffix_match() {
        let dir = fresh_temp_dir("pk3suffix");
        make_pk3(&dir, "pak0.pk3", &[("maps/mp/dm.gsc", "main() {}\n")]);
        let index = IncludeIndex::load_or_build(None, &[dir]);
        let hits = index.lookup(None, "dm");
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn missing_script_gives_no_hits() {
        let dir = fresh_temp_dir("pk3miss");
        make_pk3(&dir, "pak0.pk3", &[("maps/mp/dm.gsc", "main() {}\n")]);
        let index = IncludeIndex::load_or_build(None, &[dir]);
        assert!(index.lookup(Some("maps/mp/"), "nosuchscript").is_empty());
    }

    #[test]
    fn later_include_path_wins() {
        let first = fresh_temp_dir("pk3prec_first");
        let second = fresh_temp_dir("pk3prec_second");
        make_pk3(&first, "pak0.pk3", &[("maps/mp/dm.gsc", "old() {}\n")]);
        make_loose(&second, "maps/mp/dm.gsc", "new() {}\n");
        let index = IncludeIndex::load_or_build(None, &[first, second.clone()]);
        let hits = index.lookup(Some("maps/mp/"), "dm");
        assert_eq!(hits.len(), 2);
        let text = index.read_script_text(&hits[0]).unwrap();
        assert_eq!(text, "new() {}\n");
    }

    #[test]
    fn later_archive_wins_within_one_path() {
        let dir = fresh_temp_dir("pk3prec_arc");
        make_pk3(&dir, "pak0.pk3", &[("maps/mp/dm.gsc", "old() {}\n")]);
        make_pk3(&dir, "pak1.pk3", &[("maps/mp/dm.gsc", "new() {}\n")]);
        let index = IncludeIndex::load_or_build(None, &[dir]);
        let hits = index.lookup(Some("maps/mp/"), "dm");
        assert_eq!(hits.len(), 2);
        let text = index.read_script_text(&hits[0]).unwrap();
        assert_eq!(text, "new() {}\n");
    }

    #[test]
    fn mixed_case_extension_is_indexed() {
        let dir = fresh_temp_dir("pk3mixedext");
        make_pk3(&dir, "pak0.pk3", &[("maps/mp/dm.Gsc", "main() {}\n")]);
        let index = IncludeIndex::load_or_build(None, &[dir]);
        assert_eq!(index.entry_count(), 1);
        assert_eq!(index.lookup(Some("maps/mp/"), "dm").len(), 1);
    }

    #[test]
    fn cache_roundtrip_reuses_entries() {
        let root = fresh_temp_dir("pk3cache_ws");
        let games = fresh_temp_dir("pk3cache_games");
        make_pk3(&games, "pak0.pk3", &[("maps/mp/dm.gsc", "main() {}\n")]);
        make_loose(&games, "maps/mp/other.gsc", "main() {}\n");
        let first = IncludeIndex::load_or_build(Some(&root), std::slice::from_ref(&games));
        assert_eq!(first.entry_count(), 2);
        let cache_file = root.join(".cache/brenz/include_index.ron");
        assert!(cache_file.exists());
        let second = IncludeIndex::load_or_build(Some(&root), &[games]);
        assert_eq!(second.entry_count(), 2);
        assert_eq!(second.lookup(None, "dm").len(), 1);
        assert_eq!(second.lookup(None, "other").len(), 1);
    }

    #[test]
    fn changed_include_paths_invalidate_cache() {
        let root = fresh_temp_dir("pk3cache_inv_ws");
        let games_a = fresh_temp_dir("pk3cache_inv_a");
        let games_b = fresh_temp_dir("pk3cache_inv_b");
        make_pk3(&games_a, "a.pk3", &[("maps/a.gsc", "main() {}\n")]);
        make_pk3(&games_b, "b.pk3", &[("maps/b.gsc", "main() {}\n")]);
        let first = IncludeIndex::load_or_build(Some(&root), &[games_a]);
        assert_eq!(first.entry_count(), 1);
        let second = IncludeIndex::load_or_build(Some(&root), &[games_b]);
        assert_eq!(second.entry_count(), 1);
        assert!(second.lookup(None, "a").is_empty());
        assert_eq!(second.lookup(None, "b").len(), 1);
    }
}
