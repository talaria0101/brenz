//! Code navigation related methods for Backend

use super::Backend;
use crate::include::IncludeEntry;
use crate::util;
use std::collections::HashSet;
use std::path::PathBuf;
use tower_lsp::UriExt;
use tower_lsp::lsp_types::*;
use tower_lsp_server as tower_lsp;
use tree_sitter::Node;
use util::{LogType, logprint};

/// URI scheme for archive members. The path embeds the absolute
/// archive path plus the member path, e.g.
/// `pk3:///games/cod/main/pak0.pk3/maps/mp/gametypes/dm.gsc`, so every
/// script has a unique, debuggable identity without extracting copies.
/// Loose files keep plain file URIs, which clients can open directly.
const PK3_URI_SCHEME: &str = "pk3";

/// Percent-encode a URI path, keeping `/` and `:` (drive letters) intact.
fn encode_uri_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/' | b':') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn script_uri_for(entry: &IncludeEntry) -> Option<Uri> {
    // Builds the identity URI for an indexed script: `pk3://` for
    // archive members (archive path plus member path, escaped), or
    // a plain file URI for loose files. Returns `None` only for
    // non-UTF-8 archive paths, which cannot live in a URI.
    match &entry.source {
        crate::include::ScriptSource::Archive { pk3, inner } => {
            let pk3 = pk3.to_str()?;
            let inner = inner.replace('\\', "/");
            format!(
                "{}://{}/{}",
                PK3_URI_SCHEME,
                encode_uri_path(pk3),
                encode_uri_path(&inner)
            )
            .parse::<Uri>()
            .ok()
        }
        crate::include::ScriptSource::Loose { path } => Uri::from_file_path(path),
    }
}

impl Backend {
    /// Climb parents until a node of this kind (or run out of tree).
    pub fn find_parent_of_kind<'a>(&self, mut node: Node<'a>, kind: &str) -> Option<Node<'a>> {
        loop {
            if node.kind() == kind {
                return Some(node);
            }
            node = node.parent()?;
        }
    }

    // pub fn find_parent_of_kind_b4_kind<'a>(&self, mut node: Node<'a>, kind: &str, until: &str) -> Option<Node<'a>>
    // {
    //     loop {
    //         if node.kind() == kind {
    //             return Some(node);
    //         }
    //         else if node.kind() == until {
    //             return None;
    //         }
    //         node = node.parent()?;
    //     }
    // }

    /// Direct children only; use `find_descendants_of_kind` for deep search.
    pub fn find_child_of_kind<'a>(&self, node: Node<'a>, kind: &str) -> Option<Node<'a>> {
        let mut cursor = node.walk();
        node.children(&mut cursor)
            .find(|child| child.kind() == kind)
    }

    /*pub fn find_children_of_kind<'a>(&self, node: Node<'a>, kind: &str) -> Vec<Node<'a>>
    {
        let mut children: Vec<Node> = Vec::new();
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == kind {
                children.push(child);
            }
        }

        children
    }*/

    /// Every descendant of this kind, in tree order. The workhorse
    /// behind call-site collection and reference hunts.
    pub fn find_descendants_of_kind<'a>(node: Node<'a>, kind: &str, results: &mut Vec<Node<'a>>) {
        if node.kind() == kind {
            results.push(node);
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            Self::find_descendants_of_kind(child, kind, results);
        }
    }

    /**
     * Returns
     * - `path` of the script (if exists)
     * - `name` of the script
     * - `function` called or referenced
     *
     * of a `foreign_func_ref` or `foreign_call_expression` node
     **/
    pub(crate) fn process_foreign_fn(
        &self,
        node: Node,
        source: &str,
    ) -> Option<(Option<String>, String, String)> {
        let path = node
            .child_by_field_name("path")
            .map(|p| source[p.start_byte()..p.end_byte()].to_string());
        let script_node = node.child_by_field_name("script")?;
        let function_node = node.child_by_field_name("function")?;

        let script_name = source[script_node.start_byte()..script_node.end_byte()].to_string();
        let function_name =
            source[function_node.start_byte()..function_node.end_byte()].to_string();

        Some((path, script_name, function_name))
    }

    /// Resolve a call to its definition: foreign scripts first, then
    /// same-file functions. Returns the location plus doc comment.
    pub(crate) async fn resolve_target_fn(
        &self,
        start_node: Node<'_>,
        src: &str,
        current_uri: &Uri,
        comment: bool,
    ) -> Option<(Location, Option<String>)> {
        if let Some(res) = self
            .resolve_foreign_target(start_node, src, current_uri, comment)
            .await
        {
            return Some(res);
        }

        if let Some(res) = self
            .resolve_local_target(start_node, src, current_uri, comment)
            .await
        {
            return Some(res);
        }
        None
    }

    async fn resolve_foreign_target(
        &self,
        node: Node<'_>,
        src: &str,
        _current_uri: &Uri,
        comment: bool,
    ) -> Option<(Location, Option<String>)> {
        if let Some(foreign_ref) = self.find_child_of_kind(node, "foreign_function_ptr")
            && let Some((p, s, f)) = self.process_foreign_fn(foreign_ref, src)
        {
            logprint!(LogType::Info, "past processing: {:#?}, {}, {}", &p, &s, &f);
            match self.ensure_script_loaded(p.as_deref(), &s).await {
                Some(uri) => {
                    if let Some((rng, cmt)) = self.get_function(&uri, f.clone(), comment).await {
                        return Some((Location { uri, range: rng }, cmt));
                    }
                    logprint!(LogType::Error, "Function {f} not found in script {s}");
                }
                None => {
                    let msg = format!("Can't determine script path for: {s}");
                    util::logprint!(LogType::Error, "{msg}");
                    self.client.log_message(MessageType::ERROR, msg).await;
                }
            }
        }
        None
    }

    /// Make sure the referenced script is loaded, from the workspace or
    /// from a `.pk3` archive, along with its own dependencies.
    ///
    /// Returns the URI the script is stored under. Works iteratively with
    /// an explicit queue, so dependency cycles terminate.
    ///
    // `Uri` carries interior mutability but is never mutated in place
    // here; it is only ever an immutable visited marker.
    #[allow(clippy::mutable_key_type)]
    pub(crate) async fn ensure_script_loaded(
        &self,
        path: Option<&str>,
        script: &str,
    ) -> Option<Uri> {
        // Timed transitive load: one call pulls the target plus every
        // script it references, so this total is the number hover and
        // diagnostics wait on after an open.
        let total_start = std::time::Instant::now();
        let first_path = path.map(str::to_string);
        let first_script = script.to_string();
        let mut visited: HashSet<Uri> = HashSet::new();
        let mut target: Option<Uri> = None;
        let mut queue: Vec<(Option<String>, String)> = vec![(first_path, first_script.clone())];
        let mut loads = 0usize;
        let mut revisited = 0usize;

        while let Some((p, s)) = queue.pop() {
            let Some(uri) = self.load_one_script(p.as_deref(), &s).await else {
                continue;
            };
            loads += 1;
            if target.is_none() {
                target = Some(uri.clone());
            }
            if !visited.insert(uri.clone()) {
                revisited += 1;
                continue;
            }
            queue.extend(self.foreign_refs_of(&uri).await);
        }

        logprint!(
            LogType::Info,
            "Brenz timing: ensure_script_loaded script={} loads={} revisited={} visited={} total={}ms found={}",
            first_script,
            loads,
            revisited,
            visited.len(),
            crate::util::timing_ms(total_start),
            target.is_some()
        );

        target
    }

    /// The base for workspace script lookups: the configured `root`
    /// when set (relative to the workspace), else the workspace
    /// itself. Workspace files always win over include lookups.
    pub(crate) async fn script_root(&self) -> Option<PathBuf> {
        let workspace = self.workspace_root.lock().await.clone();
        let cfg = self.config.lock().await;
        match &cfg.root {
            Some(r) if r.is_absolute() => Some(r.clone()),
            Some(r) => workspace.as_ref().map(|w| w.join(r)),
            None => workspace,
        }
    }

    /// Load one script if needed and return its URI: a workspace file is
    /// read from disk when it has not been opened yet, otherwise the
    /// include index (loose files, then `.pk3` archives) is searched
    /// case-insensitively.
    async fn load_one_script(&self, path: Option<&str>, script: &str) -> Option<Uri> {
        let load_start = std::time::Instant::now();
        let root = self.script_root().await;
        if let Ok(abs) =
            util::resolove_scr_path(&root, path.map(str::to_string), script.to_string())
            && abs.is_file()
        {
            match std::fs::read_to_string(&abs) {
                Ok(text) => {
                    let uri = Uri::from_file_path(&abs)?;
                    let fresh = !self.fn_defs.lock().await.contains_key(&uri);
                    if fresh {
                        self.parse_and_store(uri.clone(), &text).await;
                    }
                    logprint!(
                        LogType::Info,
                        "Brenz timing: load_one_script src=workspace fresh={} bytes={} total={}ms script={} uri={}",
                        fresh,
                        text.len(),
                        crate::util::timing_ms(load_start),
                        script,
                        uri.as_str()
                    );
                    return Some(uri);
                }
                Err(e) => {
                    // Fall through to the include lookup below.
                    logprint!(
                        LogType::Error,
                        "Couldn't read {}: {e}, trying include paths",
                        abs.display()
                    );
                }
            }
        }

        let t = std::time::Instant::now();
        let entry = self
            .include_index
            .lock()
            .await
            .lookup(path, script)
            .into_iter()
            .next()?;
        let lookup_ms = crate::util::timing_ms(t);
        let uri = script_uri_for(&entry)?;
        let fresh = !self.fn_defs.lock().await.contains_key(&uri);
        if fresh {
            let t = std::time::Instant::now();
            let text = match self.include_index.lock().await.read_script_text(&entry) {
                Ok(t) => t,
                Err(e) => {
                    logprint!(LogType::Error, "Couldn't read {}: {e}", entry.display());
                    return None;
                }
            };
            let read_ms = crate::util::timing_ms(t);
            logprint!(
                LogType::Info,
                "Loaded {} (read={}ms)",
                entry.display(),
                read_ms
            );
            if !self.parse_and_store(uri.clone(), &text).await {
                return None;
            }
            logprint!(
                LogType::Info,
                "Brenz timing: load_one_script src=include fresh=true lookup={}ms read={}ms bytes={} total={}ms script={} uri={}",
                lookup_ms,
                read_ms,
                text.len(),
                crate::util::timing_ms(load_start),
                script,
                uri.as_str()
            );
        } else {
            logprint!(
                LogType::Info,
                "Brenz timing: load_one_script src=include fresh=false lookup={}ms total={}ms script={} uri={}",
                lookup_ms,
                crate::util::timing_ms(load_start),
                script,
                uri.as_str()
            );
        }
        Some(uri)
    }

    /// Foreign script references made by an already-loaded script.
    async fn foreign_refs_of(&self, uri: &Uri) -> Vec<(Option<String>, String)> {
        let (tree, src) = {
            let trees = self.trees.lock().await;
            let tree = match trees.get(uri) {
                Some(t) => t.clone(),
                None => return Vec::new(),
            };
            let dc = self.docs_content.lock().await;
            let src = match dc.get(uri) {
                Some(s) => s.clone(),
                None => return Vec::new(),
            };
            (tree, src)
        };

        let mut nodes = Vec::new();
        Self::find_descendants_of_kind(tree.root_node(), "foreign_function_ptr", &mut nodes);
        let mut refs = Vec::new();
        for n in nodes {
            if let Some((p, s, _)) = self.process_foreign_fn(n, &src)
                && !refs.contains(&(p.clone(), s.clone()))
            {
                refs.push((p, s));
            }
        }
        refs
    }

    /// Same-file `foo()` and `::foo` lookup in this document's table.
    async fn resolve_local_target(
        &self,
        node: Node<'_>,
        src: &str,
        current_uri: &Uri,
        comment: bool,
    ) -> Option<(Location, Option<String>)> {
        let func = match self.find_child_of_kind(node, "local_function_ptr") {
            Some(child) => child.child_by_field_name("function"),
            None => node.child_by_field_name("function"),
        };
        if let Some(func) = func {
            let fn_name = src[func.start_byte()..func.end_byte()].to_string();
            if let Some((rng, cmt)) = self.get_function(current_uri, fn_name, comment).await {
                return Some((
                    Location {
                        uri: current_uri.clone(),
                        range: rng,
                    },
                    cmt,
                ));
            }
        }
        None
    }

    /// Latest assignment to a variable before a position, for
    /// go-to-definition on locals. Walks the enclosing function and
    /// remembers the last write it saw.
    pub(crate) fn find_var_def<'a>(
        &self,
        identifier: &str,
        current_node: Node<'a>,
        source: &str,
    ) -> Option<Node<'a>> {
        // Find containing function
        let function = self.find_parent_of_kind(current_node, "function_definition")?;

        let mut stop_at = current_node.start_byte();

        if let Some(assign_node) = self.find_parent_of_kind(current_node, "assignment_expression")
            && let Some(assigned_value) = assign_node.child_by_field_name("assigned_value")
            && assigned_value
                .byte_range()
                .contains(&current_node.start_byte())
        {
            stop_at = assign_node.start_byte()
        }

        let mut last_assignment = None;

        // Walk function body looking for assignments before current position
        self.walk_tree_for_assignment(function, identifier, stop_at, source, &mut last_assignment);

        last_assignment
    }

    // Depth-first hunt for writes to one variable, stopping at the
    // cursor. Later writes overwrite earlier ones in `result`, so
    // what comes back is the latest write before the position.
    fn walk_tree_for_assignment<'a>(
        &self,
        node: Node<'a>,
        target_var: &str,
        stop_at: usize,
        source: &str,
        result: &mut Option<Node<'a>>,
    ) {
        if node.start_byte() >= stop_at {
            return; // Past our position
        }

        if node.kind() == "assignment_expression"
            && let Some(var_node) = node.child_by_field_name("variable")
        {
            let var_name = &source[var_node.start_byte()..var_node.end_byte()];
            if var_name == target_var {
                *result = Some(node);
            }
        }

        // Recurse into children
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.walk_tree_for_assignment(child, target_var, stop_at, source, result);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entry(pk3: &str, inner: &str) -> IncludeEntry {
        IncludeEntry {
            source: crate::include::ScriptSource::Archive {
                pk3: PathBuf::from(pk3),
                inner: inner.to_string(),
            },
            dir_order: 0,
        }
    }

    fn loose_entry(path: &str) -> IncludeEntry {
        IncludeEntry {
            source: crate::include::ScriptSource::Loose {
                path: PathBuf::from(path),
            },
            dir_order: 0,
        }
    }

    #[test]
    fn pk3_uris_parse_and_stay_unique() {
        let a = script_uri_for(&entry("/games/cod/main/pak0.pk3", "maps/mp/dm.gsc")).unwrap();
        let b = script_uri_for(&entry("/games/cod/main/pak1.pk3", "maps/mp/dm.gsc")).unwrap();
        let c = script_uri_for(&entry("/games/cod/main/pak0.pk3", "maps\\MP\\DM.GSC")).unwrap();
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert!(a.as_str().starts_with("pk3:///games/cod/main/pak0.pk3/"));
    }

    #[test]
    fn pk3_uris_encode_spaces() {
        let u = script_uri_for(&entry("/games/my game/pak0.pk3", "maps/mp/a b.gsc")).unwrap();
        assert!(u.as_str().contains("%20"));
    }

    #[test]
    fn loose_files_get_file_uris() {
        let u = script_uri_for(&loose_entry("/games/cod/maps/mp/dm.gsc")).unwrap();
        assert_eq!(u.as_str(), "file:///games/cod/maps/mp/dm.gsc");
    }
}
