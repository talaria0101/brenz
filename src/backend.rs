//! Backend for our language server
use tower_lsp::Client;
use tower_lsp::lsp_types::*;
use tree_sitter::{Parser, Tree, Node};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::Mutex;
use std::fmt::Debug;

use crate::brace::BracketChecker;

pub struct Backend {
    /// The tower_lsp::Client instance
    pub client: Client,
    /// The BracketChecker instance
    ///
    /// TODO: Give this guy a better name, merge requests are welcome
    checker: Mutex<BracketChecker>,
    /// The Parser instance
    parser: Mutex<Parser>,
    /// Your workspace/project folder
    pub workspace_root: Mutex<Option<PathBuf>>,
    /// Stores parsed trees for each document
    pub trees: Mutex<HashMap<Url, Tree>>,
    /// Function Definitions
    pub fn_defs: Mutex<HashMap<Url, HashMap<String, Range>>>,
    /// For storing text content of scripts
    pub docs_content: Mutex<HashMap<Url, String>>,
}

impl Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backend")
        .field("client", &self.client)
        .field("trees", &self.trees)
        .finish()
    }
}

impl Backend {
    pub fn new(client: Client) -> Self
    {
        let language = tree_sitter_gsc::LANGUAGE.into();
        let mut parser = Parser::new();
        parser.set_language(&language).expect("Error loading GSC language");

        Self {
            client,
            checker: Mutex::new(BracketChecker::new()),
            parser: Mutex::new(parser),
            workspace_root: Mutex::new(None),
            trees: Mutex::new(HashMap::new()),
            fn_defs: Mutex::new(HashMap::new()),
            docs_content: Mutex::new(HashMap::new()),
        }
    }

    pub async fn parse_and_diagnose(&self, uri: Url, text: &str)
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

                self.client.publish_diagnostics(uri.clone(), diagnostics, None).await;

                let fns = self.extract_fns(&tree, text);
                self.fn_defs.lock().await.insert(uri, fns);
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
                if let Some(child) = node.child(i) {
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
            if let Some(child) = node.child(i) {
                self.find_errors(&child, source, diagnostics);
            }
        }
    }

    /// Get function definitions from parsed trees
    fn extract_fns(&self, tree: &Tree, source: &str) -> HashMap<String, Range>
    {
        let mut fns: HashMap<String, Range> = HashMap::new();
        let root = tree.root_node();
        let mut cursor = root.walk();

        for child in root.children(&mut cursor) {
            // First child should be function name identifier
            if let Some(name_node) = child.child(0) {
                if name_node.kind() == "identifier" {
                    let name = &source[name_node.start_byte()..name_node.end_byte()];
                    let range = Range {
                        start: self.byte_to_position(source, name_node.start_byte()),
                        end: self.byte_to_position(source, name_node.end_byte()),
                    };

                    fns.insert(name.to_string(), range);
                }
            }
        }
        fns
    }

    /// Helper to find node at position
    pub fn node_at_pos<'a>(&self, tree: &'a Tree, source: &str, position: Position) -> Option<Node<'a>>
    {
        let byte_offset = self.position_to_byte(source, position);
        let root = tree.root_node();

        /*match  {
            Ok(n) => Some(n),
            Err(e) => {
                self.client.log_message(MessageType::LOG, format!("No node at {:?}", position));
                None
            }
        }*/

        root.descendant_for_byte_range(byte_offset, byte_offset)
    }

    pub fn find_parent_of_kind<'a>(&self, mut node: Node<'a>, kind: &str) -> Option<Node<'a>>
    {
        loop {
            if node.kind() == kind {
                return Some(node);
            }
            node = node.parent()?;
        }
    }

    pub fn find_child_of_kind<'a>(&self, node: Node<'a>, kind: &str) -> Option<Node<'a>>
    {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == kind {
                return Some(child);
            }
        }
        None
    }

    /**
     Returns
     - `path` of the script (if exists)
     - `name` of the script
     - `function` called or referenced

     of a `foreign_func_ref` or `foreign_call_expression` node
    **/
    pub fn process_foreign_fn(&self, node: Node, source: &str) -> Option<(Option<String>, String, String)>
    {
        let path = match node.child_by_field_name("path") {
            Some(p) => {
                Some(&source[p.start_byte()..p.end_byte()].to_string())
            }
            None => None,
        };
        let script_node = node.child_by_field_name("script")?;
        let function_node = node.child_by_field_name("function")?;

        let script_name = &source[script_node.start_byte()..script_node.end_byte()];
        let function_name = &source[function_node.start_byte()..function_node.end_byte()];

        Some((path.cloned(), script_name.to_string(), function_name.to_string()))
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

    fn byte_to_position(&self, source: &str, byte_offset: usize) -> Position
    {
        let mut line = 0;
        let mut character = 0;

        for (i, ch) in source.char_indices() {
            if i >= byte_offset {
                break;
            }

            if ch == '\n' {
                line += 1;
                character = 0;
            } else {
                character += 1;
            }
        }

        Position { line, character }
    }

    fn position_to_byte(&self, source: &str, position: Position) -> usize
    {
        let mut byte_offset = 0;
        let mut current_line = 0;
        let mut current_char = 0;

        for ch in source.chars() {
            if current_line == position.line && current_char == position.character {
                return byte_offset;
            }

            if ch == '\n' {
                current_line += 1;
                current_char = 0;
            } else {
                current_char += 1;
            }

            byte_offset += ch.len_utf8();
        }

        byte_offset
    }
}
