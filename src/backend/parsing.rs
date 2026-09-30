//! Parsing related methods for Backend

use crate::compiler::compile::compile as compile_gsc;
use crate::util::{LogType, logprint};

use super::Backend;
use std::collections::HashMap;
use tower_lsp::lsp_types::*;
use tower_lsp_server as tower_lsp;
use tree_sitter::Tree;

impl Backend {
    /// The whole pipeline for one document version: brackets, parse,
    /// symbols, compile checks, project checks, then publish. Runs on
    /// open and (debounced) on every edit.
    pub async fn parse_and_diagnose(&self, uri: Uri, text: &str) {
        let checker = self.checker.clone();
        let parser = self.parser.clone();
        let trees = self.trees.clone();
        let fn_defs = self.fn_defs.clone();
        let sym_defs = self.sym_defs.clone();
        let client = self.client.clone();
        //let mut checker = BracketChecker::new();
        let mut checker_guard = checker.lock().await;
        let bracket_errors = checker_guard.check(text);
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

            client.publish_diagnostics(uri, diagnostics, None).await;
            return;
        }

        // Parse in a narrow scope: the project-aware checks below
        // re-lock the parser for on-demand files, and holding this
        // guard across them deadlocks the task on itself.
        let tree = parser.lock().await.parse(text, None);
        match tree {
            Some(tree) => {
                {
                    trees.lock().await.insert(uri.clone(), tree.clone());

                    let mut diagnostics = Self::collect_diagnostics(&tree, text);

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
                        let fns = Self::extract_fns(&tree, text);
                        fn_defs.lock().await.insert(uri.clone(), fns);
                        let syms = Self::extract_syms(&tree, text);
                        sym_defs.lock().await.insert(uri.clone(), syms);
                        // Structural compile check: engine operations
                        // still compile, so failures here are real bugs
                        // (break outside loops, uncompilable nodes).
                        if let Err(errors) = compile_gsc(&tree, text) {
                            diagnostics = errors.into_iter().map(|e| e.into()).collect();
                        }
                        diagnostics.extend(self.script_diagnostics(&uri).await);
                    }

                    client.publish_diagnostics(uri, diagnostics, None).await;
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
                    .publish_diagnostics(uri, vec![diagnostic], None)
                    .await;
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
        let tree = { self.parser.lock().await.parse(text, None) };
        let Some(tree) = tree else {
            return false;
        };
        let fns = Self::extract_fns(&tree, text);
        let syms = Self::extract_syms(&tree, text);
        self.trees.lock().await.insert(uri.clone(), tree);
        self.fn_defs.lock().await.insert(uri.clone(), fns);
        self.sym_defs.lock().await.insert(uri.clone(), syms);
        self.docs_content.lock().await.insert(uri, text.to_string());
        true
    }

    /// Get function definitions from parsed trees
    pub(crate) fn extract_fns(tree: &Tree, source: &str) -> super::FnDefs {
        let mut fns: super::FnDefs = HashMap::new();
        let root = tree.root_node();
        let mut cursor = root.walk();

        for child in root.children(&mut cursor) {
            // First child should be function name identifier
            if let Some(func_head) = child.child_by_field_name("func_head")
                && let Some(name_node) = Self::first_code_child(&func_head)
                && name_node.kind() == "identifier"
            {
                let name = &source[name_node.start_byte()..name_node.end_byte()];
                let range = Range {
                    start: Self::byte_to_position(source, func_head.start_byte()),
                    end: Self::byte_to_position(source, func_head.end_byte()),
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

                logprint!(
                    LogType::Info,
                    "Name: {}, Function: {}, Comment: {:?}",
                    name,
                    &source[func_head.start_byte()..func_head.end_byte()],
                    &comment.as_ref()
                );

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

        for child in root.children(&mut cursor) {
            if child.kind() == "function_definition" {
                let mut vars: Vec<DocumentSymbol> = Vec::new();

                if let Some(func_block) = child.child_by_field_name("func_block") {
                    logprint!(LogType::Info, "in func_block");
                    let mut assign_exprs = Vec::new();
                    Self::find_descendants_of_kind(
                        func_block,
                        "assignment_expression",
                        &mut assign_exprs,
                    );
                    logprint!(LogType::Info, "assign_exprs: {:#?}", assign_exprs.clone());
                    let mut done: Vec<String> = Vec::new();

                    for expr in assign_exprs.into_iter() {
                        if let Some(var) = expr.child_by_field_name("variable") {
                            let var_name = &src[var.start_byte()..var.end_byte()];
                            // don't want dups in symbol tree
                            if done.contains(&var_name.to_string()) {
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
                                    start: Self::byte_to_position(src, expr.start_byte()),
                                    end: Self::byte_to_position(src, expr.end_byte()),
                                },
                                selection_range: Range {
                                    start: Self::byte_to_position(src, var.start_byte()),
                                    end: Self::byte_to_position(src, var.end_byte()),
                                },
                                children: None,
                            };

                            //logprint!(LogType::Info, "Symbol: {:#?}", sym.clone());

                            vars.push(sym);

                            done.push(var_name.to_string());
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
                            start: Self::byte_to_position(src, child.start_byte()),
                            end: Self::byte_to_position(src, child.end_byte()),
                        },
                        selection_range: Range {
                            start: Self::byte_to_position(src, func_head.start_byte()),
                            end: Self::byte_to_position(src, func_head.end_byte()),
                        },
                        children: Some(vars),
                    });
                }
            }
        }

        symbols
    }
}
