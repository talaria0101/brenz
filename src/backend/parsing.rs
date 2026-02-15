//! Parsing related methods for Backend

use crate::util::{logprint, LogType};

use tower_lsp_server as tower_lsp;
use super::Backend;
use tower_lsp::lsp_types::*;
use tree_sitter::Tree;
use std::collections::HashMap;

impl Backend {
    pub async fn parse_and_diagnose(&self, uri: Uri, text: &str)
    {
        let mut checker = self.checker.lock().await;
        let bracket_errors = checker.check(text);
        if !bracket_errors.is_empty() {
            let diagnostics: Vec<Diagnostic> = bracket_errors
            .into_iter()
            .map(|error| error.into())
            .collect();

            for (i, diag) in diagnostics.iter().enumerate() {
                self.client.log_message(
                    MessageType::INFO,
                    format!("Bracket or Quote Error {}: {}:{}-{}:{} | {}",
                        i,
                        diag.range.start.line, diag.range.start.character,
                        diag.range.end.line, diag.range.end.character,
                        diag.message
                    )
                ).await;
            }

            self.client.publish_diagnostics(uri, diagnostics, None).await;
            return;
        }
        let mut parser = self.parser.lock().await;

        match parser.parse(text, None) {
            Some(tree) => {
                self.trees.lock().await.insert(uri.clone(), tree.clone());

                let mut diagnostics = self.collect_diagnostics(&tree, text);

                if !diagnostics.is_empty() {
                    for (i, diag) in diagnostics.iter_mut().enumerate() {
                        self.client.log_message(
                            MessageType::INFO,
                            format!("Diagnostic {}: {}:{}-{}:{} | {}",
                                i,
                                diag.range.start.line, diag.range.start.character,
                                diag.range.end.line, diag.range.end.character,
                                diag.message
                            )
                        ).await;
                        if &diag.message == "Expected ;" {
                            diag.message = "Perhaps you forgot a semi-colon (;) here?".to_string();
                        }
                    }
                }
                else {
                    let fns = self.extract_fns(&tree, text);
                    self.fn_defs.lock().await.insert(uri.clone(), fns);
                    let syms = self.extract_syms(&tree, text);
                    self.sym_defs.lock().await.insert(uri.clone(), syms);
                }

                self.client.publish_diagnostics(uri, diagnostics, None).await;
            }
            None => {
                let diagnostic = Diagnostic {
                    range: Range {
                        start: Position { line: 0, character: 0 },
                        end: Position { line: 0, character: text.chars().count() as u32 },
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

                self.client.publish_diagnostics(uri, vec![diagnostic], None).await;
            }
        }
    }

    fn collect_diagnostics(&self, tree: &Tree, source: &str) -> Vec<Diagnostic>
    {
        let mut diagnostics = Vec::new();

        // Walk the tree looking for ERROR nodes
        self.find_errors(&tree.root_node(), source, &mut diagnostics);

        diagnostics
    }

    fn find_errors(&self, node: &tree_sitter::Node, source: &str, diagnostics: &mut Vec<Diagnostic>)
    {
        if node.kind() == "ERROR" {
            let start_pos = self.byte_to_position(source, node.start_byte());
            let mut end_pos = self.byte_to_position(source, node.end_byte());

            // Need to make sure we have at least a single character range for visibility
            if start_pos == end_pos {
                end_pos.character += 1;
            }

            // Handle EOL cases where Kate might not show underlines
            let (adjusted_start, adjusted_end) = self.adjust_range_for_visibility(
                source, start_pos, end_pos
            );

            let error_text = &source[node.start_byte()..node.end_byte()];
            let clean_text = error_text.replace(['\n', '\r', '\t'], " ").trim().to_string();

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
                if let Some(child) = node.child(i.try_into().unwrap()) {
                    if child.is_missing() {
                        let start_pos = self.byte_to_position(source, child.start_byte());
                        let mut end_pos = self.byte_to_position(source, child.end_byte());

                        // For missing nodes, create a small range at the expected position
                        if start_pos == end_pos {
                            end_pos.character += 1;
                        }

                        let (adjusted_start, adjusted_end) = self.adjust_range_for_visibility(
                            source, start_pos, end_pos
                        );

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
        }

        // Check children for errors
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i.try_into().unwrap()) {
                self.find_errors(&child, source, diagnostics);
            }
        }
    }

    /// Some editors (including kate) might not show a red underline at EOL.
    /// So, we need to adjust the char range to underline.
    fn adjust_range_for_visibility(&self, source: &str, start: Position, end: Position) -> (Position, Position)
    {
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
                return (start, Position {
                    line: start.line,
                    character: start.character + 1,
                });
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

    /// Get function definitions from parsed trees
    fn extract_fns(&self, tree: &Tree, source: &str) -> HashMap<String, (Range, Option<String>)>
    {
        let mut fns: HashMap<String, (Range, Option<String>)> = HashMap::new();
        let root = tree.root_node();
        let mut cursor = root.walk();

        for child in root.children(&mut cursor) {
            // First child should be function name identifier
            if let Some(func_head) = child.child_by_field_name("func_head") {
                if let Some(name_node) = func_head.child(0) {
                    if name_node.kind() == "identifier" {
                        let name = &source[name_node.start_byte()..name_node.end_byte()];
                        let range = Range {
                            start: self.byte_to_position(source, func_head.start_byte()),
                            end: self.byte_to_position(source, func_head.end_byte()),
                        };

                        let comment = match child.prev_sibling() {
                            Some(ps) => {
                                if ps.kind() == "comment" {
                                    Some(source[ps.start_byte()..ps.end_byte()].to_string())
                                }
                                else { None }
                            }
                            None => None
                        };

                        logprint!(
                            LogType::Info, "Name: {}, Function: {}, Comment: {:?}",
                            name, &source[func_head.start_byte()..func_head.end_byte()], &comment.as_ref()
                        );

                        fns.insert(name.to_string(), (range, comment));
                    }
                }
            }
        }
        fns
    }

    fn extract_syms(&self, tree: &Tree, src: &str) -> Vec<DocumentSymbol>
    {
        let mut symbols: Vec<DocumentSymbol> = Vec::new();
        let root = tree.root_node();
        let mut cursor = root.walk();

        for child in root.children(&mut cursor) {
            if child.kind() == "function_definition" {
                let mut vars: Vec<DocumentSymbol> = Vec::new();

                if let Some(func_block) = child.child_by_field_name("func_block") {
                    logprint!(LogType::Info, "in func_block");
                    let mut assign_exprs = Vec::new();
                    self.find_descendants_of_kind(func_block, "assignment_expression", &mut assign_exprs);
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
                                    start: self.byte_to_position(&src, expr.start_byte()),
                                end: self.byte_to_position(&src, expr.end_byte()),
                                },
                                selection_range:Range {
                                    start: self.byte_to_position(&src, var.start_byte()),
                                end: self.byte_to_position(&src, var.end_byte()),
                                },
                                children: None
                            };

                            //logprint!(LogType::Info, "Symbol: {:#?}", sym.clone());

                            vars.push(sym);

                            done.push(var_name.to_string());
                        }
                    }
                }

                if let Some(func_head) = child.child_by_field_name("func_head") {
                    if let Some(name_node) = func_head.child(0) {
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
                                start: self.byte_to_position(&src, child.start_byte()),
                            end: self.byte_to_position(&src, child.end_byte()),
                            },
                            selection_range: Range {
                                start: self.byte_to_position(&src, func_head.start_byte()),
                            end: self.byte_to_position(&src, func_head.end_byte()),
                            },
                            children: Some(vars),
                        });
                    }
                }
            }
        }

        symbols
    }
}
