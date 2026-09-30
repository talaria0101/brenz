//! Project-aware script diagnostics: unknown functions and scripts.
//!
//! Bare `foo()` calls resolve to the current file or a builtin in the
//! engine, so a name found in neither is reported. Foreign
//! `path\\script::func` references resolve through the workspace and
//! the `.pk3` index (loading targets on demand, like goto-definition);
//! an unresolvable script warns, a function missing from a cleanly
//! parsed target errors. Targets that fail to parse are skipped, since
//! their definition lists are partial.

use super::Backend;
use tower_lsp::lsp_types::*;
use tower_lsp_server as tower_lsp;
use tree_sitter::Node;

impl Backend {
    /// Project-aware checks for one open document: unknown calls and
    /// unresolvable foreign references. Only runs on clean parses so
    /// half-broken trees never produce noise.
    pub(crate) async fn script_diagnostics(&self, uri: &Uri) -> Vec<Diagnostic> {
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

        let mut out = Vec::new();
        let root = tree.root_node();

        // Foreign references anywhere (calls and `::` pointers alike).
        let mut foreigns = Vec::new();
        Self::find_descendants_of_kind(root, "foreign_function_ptr", &mut foreigns);
        for node in foreigns {
            self.check_foreign_fn(node, &src, &mut out).await;
        }

        // Bare calls: engine resolves them to this file or a builtin.
        let mut calls = Vec::new();
        Self::find_descendants_of_kind(root, "direct_call", &mut calls);
        Self::find_descendants_of_kind(root, "thread_call", &mut calls);
        for node in calls {
            if self
                .find_child_of_kind(node, "foreign_function_ptr")
                .is_some()
            {
                continue;
            }
            // `thread [[f]]()` carries no name to check.
            let Some(callee) = node.child_by_field_name("function") else {
                continue;
            };
            if callee.kind() != "identifier" {
                continue;
            }
            let name = src[callee.start_byte()..callee.end_byte()].to_string();
            if self.get_function(uri, name.clone(), false).await.is_some() {
                continue;
            }
            if self.is_builtin(&name).await {
                continue;
            }
            out.push(Diagnostic {
                range: Range {
                    start: Self::byte_to_position(&src, callee.start_byte()),
                    end: Self::byte_to_position(&src, callee.end_byte()),
                },
                severity: Some(DiagnosticSeverity::ERROR),
                code: Some(NumberOrString::String("unknown-function".to_string())),
                source: Some("brenz".to_string()),
                message: format!("unknown function '{name}'"),
                related_information: None,
                tags: None,
                code_description: None,
                data: None,
            });
        }

        out
    }

    /// True when a name is a known builtin (function or method),
    /// looked up case-insensitively like the engine does.
    async fn is_builtin(&self, name: &str) -> bool {
        let key = name.to_lowercase();
        let b = self.builtins_doc.lock().await;
        b.functions.contains_key(&key) || b.methods.contains_key(&key)
    }

    /// Check one foreign reference: resolve the script (warning when
    /// it exists nowhere), then the function inside it (error when the
    /// target parsed cleanly but lacks it).
    async fn check_foreign_fn(&self, node: Node<'_>, src: &str, out: &mut Vec<Diagnostic>) {
        let Some((path, script, func)) = self.process_foreign_fn(node, src) else {
            return;
        };
        let script_node = node.child_by_field_name("script").unwrap();
        let func_node = node.child_by_field_name("function").unwrap();
        let display = match &path {
            Some(p) => format!("{}\\{script}", p.strip_suffix('\\').unwrap_or(p)),
            None => script.clone(),
        };
        let Some(target) = self.ensure_script_loaded(path.as_deref(), &script).await else {
            out.push(Self::script_diag(
                src,
                &script_node,
                DiagnosticSeverity::WARNING,
                "unknown-script",
                format!("unknown script '{display}'"),
            ));
            return;
        };
        if self
            .get_function(&target, func.clone(), false)
            .await
            .is_some()
        {
            return;
        }
        // Only blame the target when it parsed cleanly; error recovery
        // leaves definition lists partial.
        let clean = self
            .trees
            .lock()
            .await
            .get(&target)
            .is_some_and(|t| !t.root_node().has_error());
        if clean {
            out.push(Self::script_diag(
                src,
                &func_node,
                DiagnosticSeverity::ERROR,
                "unknown-function",
                format!("function '{func}' not found in '{display}'"),
            ));
        }
    }

    /// One diagnostic at a node's range, with the house source label
    /// and a machine-readable code for quickfixes to match on.
    fn script_diag(
        src: &str,
        node: &Node,
        severity: DiagnosticSeverity,
        code: &str,
        message: String,
    ) -> Diagnostic {
        Diagnostic {
            range: Range {
                start: Self::byte_to_position(src, node.start_byte()),
                end: Self::byte_to_position(src, node.end_byte()),
            },
            severity: Some(severity),
            code: Some(NumberOrString::String(code.to_string())),
            source: Some("brenz".to_string()),
            message,
            related_information: None,
            tags: None,
            code_description: None,
            data: None,
        }
    }
}

impl Backend {
    /// Lowercased identifier names inside the function definition
    /// containing `pos`, for collision-free fix generation. Compared
    /// case-insensitively: clobbering `I` with `i` would be just as bad.
    fn function_identifiers(
        &self,
        tree: &tree_sitter::Tree,
        src: &str,
        pos: usize,
    ) -> std::collections::HashSet<String> {
        let mut out = std::collections::HashSet::new();
        let mut funcs = Vec::new();
        Self::find_descendants_of_kind(tree.root_node(), "function_definition", &mut funcs);
        let func = funcs
            .into_iter()
            .find(|n| n.start_byte() <= pos && pos <= n.end_byte());
        let Some(func) = func else {
            return out;
        };
        let mut ids = Vec::new();
        Self::find_descendants_of_kind(func, "identifier", &mut ids);
        for id in ids {
            if let Ok(text) = id.utf8_text(src.as_bytes()) {
                out.insert(text.to_lowercase());
            }
        }
        out
    }
}

/// First candidate not taken (case-insensitively), else `fallback`
/// with a counter suffix.
fn pick_name(
    taken: &std::collections::HashSet<String>,
    candidates: &[&str],
    fallback: &str,
) -> String {
    for c in candidates {
        if !taken.contains(&c.to_lowercase()) {
            return c.to_string();
        }
    }
    let mut n = 0;
    loop {
        n += 1;
        let name = format!("{fallback}{n}");
        if !taken.contains(&name) {
            return name;
        }
    }
}

impl Backend {
    /// Quickfix for the `foreach` error: rewrite the smallest
    /// `foreach` statement containing `pos` as a `for` loop over
    /// `.size`, keeping the needle assignment. Returns the range to
    /// replace and its replacement text.
    pub(crate) fn foreach_fix(
        &self,
        tree: &tree_sitter::Tree,
        src: &str,
        pos: Position,
    ) -> Option<(Range, String)> {
        let mut matches = Vec::new();
        Self::find_descendants_of_kind(tree.root_node(), "foreach_statement", &mut matches);
        let node = matches
            .into_iter()
            .filter(|n| {
                let start = Self::byte_to_position(src, n.start_byte());
                let end = Self::byte_to_position(src, n.end_byte());
                (start.line, start.character) <= (pos.line, pos.character)
                    && (pos.line, pos.character) <= (end.line, end.character)
            })
            .min_by_key(|n| n.end_byte() - n.start_byte())?;

        let needle = node.child_by_field_name("needle")?;
        let hay = node.child_by_field_name("hay_stack")?;
        let needle_text = src[needle.start_byte()..needle.end_byte()].to_string();
        let hay_text = src[hay.start_byte()..hay.end_byte()].to_string();

        let mut body = None;
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if !child.is_named() || child.kind() == "comment" {
                continue;
            }
            if child.id() == needle.id() || child.id() == hay.id() {
                continue;
            }
            body = Some(child);
        }
        let mut body = body?;
        // Strip the `statement` wrapper the grammar leaves behind.
        while body.kind() == "statement" && body.named_child_count() == 1 {
            body = body.named_child(0).unwrap();
        }

        let line_start = src[..node.start_byte()]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let base: String = src[line_start..node.start_byte()]
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let inner = format!("{base}\t");

        // Fresh names that cannot collide with the enclosing function:
        // reusing an in-scope `i` would corrupt an outer loop, and a
        // side-effecting haystack must run once, not per iteration.
        // Field nodes may carry an `expression` wrapper; see through it.
        let mut hay_atom = hay;
        while hay_atom.kind() == "expression" && hay_atom.named_child_count() == 1 {
            hay_atom = hay_atom.named_child(0).unwrap();
        }
        let taken = self.function_identifiers(tree, src, node.start_byte());
        let index_var = pick_name(&taken, &["i", "j", "k", "n", "idx"], "foreach_i");
        let (setup, hay_expr) = if hay_atom.kind() == "identifier" {
            (String::new(), hay_text.clone())
        } else {
            let hay_var = pick_name(&taken, &["hay", "array", "list"], "foreach_hay");
            (format!("{base}{hay_var} = {hay_text};\n"), hay_var)
        };

        let part = if body.kind() == "block" {
            // Verbatim inner statements between the braces.
            let (open, close) = (body.start_byte() + 1, body.end_byte().saturating_sub(1));
            let mut inner_text = src.get(open..close).unwrap_or("").to_string();
            while inner_text.starts_with('\n') || inner_text.starts_with('\r') {
                inner_text.remove(0);
            }
            while inner_text.ends_with('\n')
                || inner_text.ends_with('\r')
                || inner_text.ends_with(' ')
                || inner_text.ends_with('\t')
            {
                inner_text.pop();
            }
            inner_text
        } else {
            format!(
                "{inner}{}",
                src[body.start_byte()..body.end_byte()].trim_end()
            )
        };

        let new_text = format!(
            "{setup}{base}for ( {index_var} = 0; {index_var} < {hay_expr}.size; {index_var}++ )\n{base}{{\n{inner}{needle_text} = {hay_expr}[{index_var}];\n{part}\n{base}}}",
        );
        let range = Range {
            start: Self::byte_to_position(src, node.start_byte()),
            end: Self::byte_to_position(src, node.end_byte()),
        };
        Some((range, new_text))
    }
}
