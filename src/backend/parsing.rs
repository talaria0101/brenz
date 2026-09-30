//! Parsing related methods for Backend

use crate::compiler::compile::compile as compile_gsc;
use crate::util::{LogType, logprint};

use super::Backend;
use std::collections::{HashMap, HashSet};
use tower_lsp::lsp_types::*;
use tower_lsp_server as tower_lsp;
use tree_sitter::Tree;

impl Backend {
    /// The whole pipeline for one document version: brackets, parse,
    /// symbols, compile checks, project checks, then publish. Runs on
    /// open and (debounced) on every edit.
    pub async fn parse_and_diagnose(&self, uri: Uri, text: &str) {
        // Timed pipeline: each stage logs milliseconds so a slow open
        // shows exactly where time goes (brackets, tree-sitter parse,
        // error walk, symbol extraction, compile check, or the
        // project-aware script diagnostics that load dependencies).
        // All lines use the `Brenz timing:` prefix for easy grep in
        // /tmp/brenz_lsp.log, and work in release builds.
        let total_start = std::time::Instant::now();
        let text_len = text.len();
        let checker = self.checker.clone();
        let parser = self.parser.clone();
        let trees = self.trees.clone();
        let fn_defs = self.fn_defs.clone();
        let sym_defs = self.sym_defs.clone();
        let client = self.client.clone();
        //let mut checker = BracketChecker::new();
        let t = std::time::Instant::now();
        let mut checker_guard = checker.lock().await;
        let lock_checker_ms = crate::util::timing_ms(t);
        let t = std::time::Instant::now();
        let bracket_errors = checker_guard.check(text);
        let bracket_ms = crate::util::timing_ms(t);
        drop(checker_guard);
        if !bracket_errors.is_empty() {
            let diagnostics: Vec<Diagnostic> = bracket_errors
                .into_iter()
                .map(|error| error.into())
                .collect();

            for (i, diag) in diagnostics.iter().enumerate() {
                client
                    .log_message(
                        MessageType::INFO,
                        format!(
                            "Bracket or Quote Error {}: {}:{}-{}:{} | {}",
                            i,
                            diag.range.start.line,
                            diag.range.start.character,
                            diag.range.end.line,
                            diag.range.end.character,
                            diag.message
                        ),
                    )
                    .await;
            }

            client
                .publish_diagnostics(uri.clone(), diagnostics, None)
                .await;
            logprint!(
                LogType::Info,
                "Brenz timing: parse_and_diagnose early-exit brackets={}ms checker_lock={}ms bytes={} uri={}",
                bracket_ms,
                lock_checker_ms,
                text_len,
                uri.as_str()
            );
            return;
        }

        // Parse in a narrow scope: the project-aware checks below
        // re-lock the parser for on-demand files, and holding this
        // guard across them deadlocks the task on itself.
        let t = std::time::Instant::now();
        let tree = parser.lock().await.parse(text, None);
        let parse_ms = crate::util::timing_ms(t);
        match tree {
            Some(tree) => {
                {
                    trees.lock().await.insert(uri.clone(), tree.clone());

                    let t = std::time::Instant::now();
                    let mut diagnostics = Self::collect_diagnostics(&tree, text);
                    let collect_ms = crate::util::timing_ms(t);
                    let diag_count = diagnostics.len();

                    // Stage times below stay zero when the parse has
                    // errors and the later stages are skipped.
                    let mut extract_ms = 0;
                    let mut compile_ms = 0;
                    let mut script_diag_ms = 0;
                    if !diagnostics.is_empty() {
                        for (i, diag) in diagnostics.iter_mut().enumerate() {
                            client
                                .log_message(
                                    MessageType::INFO,
                                    format!(
                                        "Diagnostic {}: {}:{}-{}:{} | {}",
                                        i,
                                        diag.range.start.line,
                                        diag.range.start.character,
                                        diag.range.end.line,
                                        diag.range.end.character,
                                        diag.message
                                    ),
                                )
                                .await;
                            if &diag.message == "Expected ;" {
                                diag.message =
                                    "Perhaps you forgot a semi-colon (;) here?".to_string();
                            }
                        }
                    } else {
                        let t = std::time::Instant::now();
                        let fns = Self::extract_fns(&tree, text);
                        let fn_count = fns.len();
                        fn_defs.lock().await.insert(uri.clone(), fns);
                        let syms = Self::extract_syms(&tree, text);
                        let sym_count = syms.len();
                        sym_defs.lock().await.insert(uri.clone(), syms);
                        extract_ms = crate::util::timing_ms(t);
                        // Structural compile check: engine operations
                        // still compile, so failures here are real bugs
                        // (break outside loops, uncompilable nodes).
                        let t = std::time::Instant::now();
                        if let Err(errors) = compile_gsc(&tree, text) {
                            diagnostics = errors.into_iter().map(|e| e.into()).collect();
                        }
                        compile_ms = crate::util::timing_ms(t);
                        let t = std::time::Instant::now();
                        let extra = self.script_diagnostics(&uri).await;
                        script_diag_ms = crate::util::timing_ms(t);
                        let script_diag_count = extra.len();
                        diagnostics.extend(extra);
                        logprint!(
                            LogType::Info,
                            "Brenz timing: parse_and_diagnose clean fns={} syms={} script_diags={} uri={}",
                            fn_count,
                            sym_count,
                            script_diag_count,
                            uri.as_str()
                        );
                    }

                    let t = std::time::Instant::now();
                    let diag_total = diagnostics.len();
                    client
                        .publish_diagnostics(uri.clone(), diagnostics, None)
                        .await;
                    let publish_ms = crate::util::timing_ms(t);
                    let total_ms = crate::util::timing_ms(total_start);
                    logprint!(
                        LogType::Info,
                        "Brenz timing: parse_and_diagnose brackets={}ms parse={}ms collect={}ms (initial_diags={}) extract={}ms compile={}ms script_diag={}ms publish={}ms final_diags={} total={}ms bytes={} uri={}",
                        bracket_ms,
                        parse_ms,
                        collect_ms,
                        diag_count,
                        extract_ms,
                        compile_ms,
                        script_diag_ms,
                        publish_ms,
                        diag_total,
                        total_ms,
                        text_len,
                        uri.as_str()
                    );
                }
            }
            None => {
                let diagnostic = Diagnostic {
                    range: Range {
                        start: Position {
                            line: 0,
                            character: 0,
                        },
                        end: Position {
                            line: 0,
                            character: text.chars().count() as u32,
                        },
                    },
                    severity: Some(DiagnosticSeverity::ERROR),
                    code: None,
                    source: Some("gsc-parser".to_string()),
                    message: "Failed to parse GSC file".to_string(),
                    related_information: None,
                    tags: None,
                    code_description: None,
                    data: None,
                };

                client
                    .publish_diagnostics(uri.clone(), vec![diagnostic], None)
                    .await;
                logprint!(
                    LogType::Info,
                    "Brenz timing: parse_and_diagnose brackets={}ms parse={}ms result=null-tree total={}ms bytes={} uri={}",
                    bracket_ms,
                    parse_ms,
                    crate::util::timing_ms(total_start),
                    text_len,
                    uri.as_str()
                );
            }
        }
    }

    fn collect_diagnostics(tree: &Tree, source: &str) -> Vec<Diagnostic> {
        // Walk the whole tree gathering parse-error nodes.
        let mut diagnostics = Vec::new();

        Self::find_errors(&tree.root_node(), source, &mut diagnostics);

        diagnostics
    }

    fn find_errors(node: &tree_sitter::Node, source: &str, diagnostics: &mut Vec<Diagnostic>) {
        // Recursive sweep: `ERROR` nodes become diagnostics, missing
        // nodes (half-typed syntax) become "expected X" hints.
        if node.kind() == "ERROR" {
            let start_pos = Self::byte_to_position(source, node.start_byte());
            let mut end_pos = Self::byte_to_position(source, node.end_byte());

            // Need to make sure we have at least a single character range for visibility
            if start_pos == end_pos {
                end_pos.character += 1;
            }

            // Handle EOL cases where Kate might not show underlines
            let (adjusted_start, adjusted_end) =
                Self::adjust_range_for_visibility(source, start_pos, end_pos);

            let error_text = &source[node.start_byte()..node.end_byte()];
            let clean_text = error_text
                .replace(['\n', '\r', '\t'], " ")
                .trim()
                .to_string();

            let diagnostic = Diagnostic {
                range: Range {
                    start: adjusted_start,
                    end: adjusted_end,
                },
                severity: Some(DiagnosticSeverity::ERROR),
                code: Some(NumberOrString::String("parse-error".to_string())),
                source: Some("gsc-parser".to_string()),
                message: if clean_text.is_empty() {
                    "Parse error: unexpected token".to_string()
                } else {
                    format!("Parse error: unexpected '{}'", clean_text)
                },
                related_information: None,
                tags: None,
                code_description: None,
                data: None,
            };

            diagnostics.push(diagnostic);
        }

        // Check if node has missing children (incomplete parse)
        if node.has_error() && node.kind() != "ERROR" {
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i)
                    && child.is_missing()
                {
                    let start_pos = Self::byte_to_position(source, child.start_byte());
                    let mut end_pos = Self::byte_to_position(source, child.end_byte());

                    // For missing nodes, create a small range at the expected position
                    if start_pos == end_pos {
                        end_pos.character += 1;
                    }

                    let (adjusted_start, adjusted_end) =
                        Self::adjust_range_for_visibility(source, start_pos, end_pos);

                    let diagnostic = Diagnostic {
                        range: Range {
                            start: adjusted_start,
                            end: adjusted_end,
                        },
                        severity: Some(DiagnosticSeverity::ERROR),
                        code: Some(NumberOrString::String("missing-syntax".to_string())),
                        source: Some("gsc-parser".to_string()),
                        message: format!("Expected {}", child.kind()),
                        related_information: None,
                        tags: None,
                        code_description: None,
                        data: None,
                    };

                    diagnostics.push(diagnostic);
                }
            }
        }

        // Check children for errors
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                Self::find_errors(&child, source, diagnostics);
            }
        }
    }

    /// Some editors (including kate) might not show a red underline at EOL.
    /// So, we need to adjust the char range to underline.
    /// First non-comment child: a stray comment in front must not
    /// steal the function name slot.
    fn first_code_child<'a>(node: &tree_sitter::Node<'a>) -> Option<tree_sitter::Node<'a>> {
        let mut cursor = node.walk();
        node.children(&mut cursor)
            .find(|child| child.is_named() && child.kind() != "comment")
    }

    fn adjust_range_for_visibility(
        source: &str,
        start: Position,
        end: Position,
    ) -> (Position, Position) {
        let lines: Vec<&str> = source.lines().collect();

        // If we're beyond the available lines, return as-is
        if start.line as usize >= lines.len() {
            return (start, end);
        }

        let line = lines[start.line as usize];
        let line_length = line.chars().count() as u32;

        // If the start position is at or beyond the end of the line
        // Move the range to highlight the last character of the line
        if start.character >= line_length {
            if line_length > 0 {
                let new_start = Position {
                    line: start.line,
                    character: line_length - 1,
                };
                let new_end = Position {
                    line: start.line,
                    character: line_length,
                };
                return (new_start, new_end);
            } else {
                // Empty line, can't adjust much, but ensure we have a minimal range
                return (
                    start,
                    Position {
                        line: start.line,
                        character: start.character + 1,
                    },
                );
            }
        }

        if start == end {
            let new_end = Position {
                line: end.line,
                character: (end.character + 1).min(line_length),
            };
            return (start, new_end);
        }

        (start, end)
    }

    /// Parse `text` and store the tree, functions, symbols and content
    /// under `uri` without publishing diagnostics. Used for scripts that
    /// were not opened by the client: workspace files read from disk and
    /// scripts loaded from `.pk3` archives.
    pub(crate) async fn parse_and_store(&self, uri: Uri, text: &str) -> bool {
        let store_start = std::time::Instant::now();
        let t = std::time::Instant::now();
        let tree = { self.parser.lock().await.parse(text, None) };
        let parse_ms = crate::util::timing_ms(t);
        let Some(tree) = tree else {
            logprint!(
                LogType::Info,
                "Brenz timing: parse_and_store parse={}ms result=null-tree bytes={} uri={}",
                parse_ms,
                text.len(),
                uri.as_str()
            );
            return false;
        };
        let t = std::time::Instant::now();
        let fns = Self::extract_fns(&tree, text);
        let syms = Self::extract_syms(&tree, text);
        let extract_ms = crate::util::timing_ms(t);
        let fn_count = fns.len();
        let sym_count = syms.len();
        self.trees.lock().await.insert(uri.clone(), tree);
        self.fn_defs.lock().await.insert(uri.clone(), fns);
        self.sym_defs.lock().await.insert(uri.clone(), syms);
        self.docs_content
            .lock()
            .await
            .insert(uri.clone(), text.to_string());
        logprint!(
            LogType::Info,
            "Brenz timing: parse_and_store parse={}ms extract={}ms fns={} syms={} total={}ms bytes={} uri={}",
            parse_ms,
            extract_ms,
            fn_count,
            sym_count,
            crate::util::timing_ms(store_start),
            text.len(),
            uri.as_str()
        );
        true
    }

    /// Get function definitions from parsed trees
    pub(crate) fn extract_fns(tree: &Tree, source: &str) -> super::FnDefs {
        let mut fns: super::FnDefs = HashMap::new();
        let root = tree.root_node();
        let mut cursor = root.walk();
        // One line table for the whole file: positions below are
        // O(log n) each instead of rescanning from byte zero.
        let starts = Self::line_starts(source);

        for child in root.children(&mut cursor) {
            // First child should be function name identifier
            if let Some(func_head) = child.child_by_field_name("func_head")
                && let Some(name_node) = Self::first_code_child(&func_head)
                && name_node.kind() == "identifier"
            {
                let name = &source[name_node.start_byte()..name_node.end_byte()];
                let range = Range {
                    start: Self::byte_to_position_fast(&starts, source, func_head.start_byte()),
                    end: Self::byte_to_position_fast(&starts, source, func_head.end_byte()),
                };

                let comment = match child.prev_sibling() {
                    Some(ps) => {
                        if ps.kind() == "comment" {
                            Some(source[ps.start_byte()..ps.end_byte()].to_string())
                        } else {
                            None
                        }
                    }
                    None => None,
                };

                fns.insert(name.to_string(), (range, comment));
            }
        }
        fns
    }

    pub(crate) fn extract_syms(tree: &Tree, src: &str) -> Vec<DocumentSymbol> {
        // Function symbols with their local variables nested inside,
        // feeding both the symbol tree and completion.
        let mut symbols: Vec<DocumentSymbol> = Vec::new();
        let root = tree.root_node();
        let mut cursor = root.walk();
        // One line table for the whole file: every range below is
        // O(log n). The slow path rescans from byte zero per call,
        // which is quadratic on files with thousands of writes and
        // was the 6-minute open (36s on one 461KB waypoint file).
        let starts = Self::line_starts(src);

        for child in root.children(&mut cursor) {
            if child.kind() == "function_definition" {
                let mut vars: Vec<DocumentSymbol> = Vec::new();

                if let Some(func_block) = child.child_by_field_name("func_block") {
                    let mut assign_exprs = Vec::new();
                    Self::find_descendants_of_kind(
                        func_block,
                        "assignment_expression",
                        &mut assign_exprs,
                    );
                    // Borrowed set, no per-write allocation: the old
                    // `Vec::contains` scan was quadratic in the number
                    // of distinct variables.
                    let mut done: HashSet<&str> = HashSet::new();

                    for expr in assign_exprs.into_iter() {
                        if let Some(var) = expr.child_by_field_name("variable") {
                            let var_name = &src[var.start_byte()..var.end_byte()];
                            // don't want dups in symbol tree
                            if !done.insert(var_name) {
                                continue;
                            }

                            let sym = DocumentSymbol {
                                name: var_name.to_lowercase(),
                                detail: None,
                                kind: SymbolKind::VARIABLE,
                                tags: None,
                                #[allow(deprecated)]
                                deprecated: None,
                                range: Range {
                                    start: Self::byte_to_position_fast(
                                        &starts,
                                        src,
                                        expr.start_byte(),
                                    ),
                                    end: Self::byte_to_position_fast(&starts, src, expr.end_byte()),
                                },
                                selection_range: Range {
                                    start: Self::byte_to_position_fast(
                                        &starts,
                                        src,
                                        var.start_byte(),
                                    ),
                                    end: Self::byte_to_position_fast(&starts, src, var.end_byte()),
                                },
                                children: None,
                            };

                            //logprint!(LogType::Info, "Symbol: {:#?}", sym.clone());

                            vars.push(sym);
                        }
                    }
                }

                if let Some(func_head) = child.child_by_field_name("func_head")
                    && let Some(name_node) = Self::first_code_child(&func_head)
                {
                    let name = &src[name_node.start_byte()..name_node.end_byte()];
                    let detail = &src[func_head.start_byte()..func_head.end_byte()];

                    symbols.push(DocumentSymbol {
                        name: name.to_lowercase(),
                        detail: Some(detail.to_string()),
                        kind: SymbolKind::FUNCTION,
                        tags: None,
                        #[allow(deprecated)]
                        deprecated: None, // screw it
                        range: Range {
                            start: Self::byte_to_position_fast(&starts, src, child.start_byte()),
                            end: Self::byte_to_position_fast(&starts, src, child.end_byte()),
                        },
                        selection_range: Range {
                            start: Self::byte_to_position_fast(
                                &starts,
                                src,
                                func_head.start_byte(),
                            ),
                            end: Self::byte_to_position_fast(&starts, src, func_head.end_byte()),
                        },
                        children: Some(vars),
                    });
                }
            }
        }

        symbols
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> tree_sitter::Tree {
        let language = tree_sitter_gsc::LANGUAGE.into();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        parser.parse(src, None).unwrap()
    }

    #[test]
    fn fast_positions_agree_with_slow_path() {
        let src =
            "main()\n{\n\tx = 1;\n\ts = \"héllo\";\n\ty = x + 1;\n}\nfoo()\n{\n\treturn 0;\n}\n";
        let starts = Backend::line_starts(src);
        let mut off = 0;
        while off <= src.len() {
            if !src.is_char_boundary(off) {
                off += 1;
                continue;
            }
            assert_eq!(
                Backend::byte_to_position(src, off),
                Backend::byte_to_position_fast(&starts, src, off),
                "offset {off}"
            );
            off += 1;
        }
    }

    #[test]
    fn duplicate_writes_yield_one_variable_symbol() {
        let src = "main()\n{\n\tx = 1;\n\tx = 2;\n\ty = x;\n\tx = 3;\n}\n";
        let tree = parse(src);
        let syms = Backend::extract_syms(&tree, src);
        assert_eq!(syms.len(), 1);
        let kids = syms[0].children.as_ref().unwrap();
        assert_eq!(kids.len(), 2);
        assert!(kids.iter().any(|v| v.name == "x"));
        assert!(kids.iter().any(|v| v.name == "y"));
        // Function ranges still resolve through the fast path.
        let fns = Backend::extract_fns(&tree, src);
        assert_eq!(fns.len(), 1);
        assert!(fns.contains_key("main"));
    }

    #[test]
    fn thousands_of_writes_extract_quickly() {
        // Regression guard for the 6-minute open: 8000 assignments in
        // one function used to take minutes (quadratic dedupe plus a
        // full file rescan per position). Bound is generous; the fixed
        // path runs in well under a second.
        let mut src = String::from("init_waypoints()\n{\n\twp = [];\n");
        for i in 0..8000 {
            src.push_str(&format!("\twp{i} = {i};\n"));
        }
        src.push_str("}\n");
        let tree = parse(&src);
        let t = std::time::Instant::now();
        let syms = Backend::extract_syms(&tree, &src);
        let ms = t.elapsed().as_millis();
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].children.as_ref().unwrap().len(), 8001);
        assert!(ms < 30_000, "extract took {ms}ms");
    }
}

