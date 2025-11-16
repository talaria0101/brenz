//! Checks for mismatching brackets and quotes to give friendlier diagnostics.
//!
//! Mostly done by Claude

use tower_lsp_server as tower_lsp;
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};

#[derive(Debug, Clone)]
pub struct SyntaxError {
    pub range: Range,
    pub message: String,
}

/// Checks the script for mismatching brackets and quotes
///
/// TODO: Give this guy a better name, merge requests are welcome
pub struct BracketChecker {
    stack: Vec<(char, usize, Position)>, // (bracket, byte_pos, lsp_position)
    errors: Vec<SyntaxError>,
}

impl BracketChecker {
    pub fn new() -> Self {
        Self {
            stack: Vec::new(),
            errors: Vec::new(),
        }
    }

    pub fn check(&mut self, source: &str) -> Vec<SyntaxError> {
        self.stack.clear();
        self.errors.clear();

        let mut chars = source.char_indices().peekable();
        let mut in_string = false;
        let mut string_start: Option<(usize, Position)> = None;

        let mut line = 0u32;
        let mut col = 0u32;

        while let Some((pos, ch)) = chars.next() {
            let current_pos = Position::new(line, col);

            match ch {
                '\n' => {
                    // If we're in a string when we hit a newline, that's likely an unclosed string
                    if in_string {
                        if let Some((_, start_pos)) = string_start {
                            self.errors.push(SyntaxError {
                                range: Range::new(start_pos, Position::new(start_pos.line, start_pos.character + 1)),
                                             message: "Unclosed string literal - strings cannot span multiple lines".to_string(),
                            });
                            in_string = false;
                            string_start = None;
                        }
                    }
                    line += 1;
                    col = 0;
                    continue;
                }
                '"' => {
                    if in_string {
                        in_string = false;
                        string_start = None;
                    } else {
                        in_string = true;
                        string_start = Some((pos, current_pos));
                    }
                }
                '\\' if in_string => {
                    // Skip escaped character
                    if let Some((_, next_ch)) = chars.next() {
                        if next_ch == '\n' {
                            line += 1;
                            col = 0;
                        } else {
                            col += 1;
                        }
                    }
                }
                '(' | '[' | '{' if !in_string => {
                self.stack.push((ch, pos, current_pos));
                }
                ')' | ']' | '}' if !in_string => {
                    self.check_closing_bracket(ch, pos, current_pos);
                }
                _ => {}
            }

            col += ch.len_utf8() as u32;
        }

        // Check for unclosed brackets and strings
        self.check_unclosed_first();
        if let Some((_, start_pos)) = string_start {
            self.errors.push(SyntaxError {
                range: Range::new(start_pos, Position::new(line, col)),
                message: "Unclosed string literal".to_string(),
            });
        }

        std::mem::take(&mut self.errors)
    }

    fn check_closing_bracket(&mut self, ch: char, _pos: usize, current_pos: Position) {
        let expected = match ch {
            ')' => '(',
            ']' => '[',
            '}' => '{',
            _ => return,
        };

        match self.stack.pop() {
            Some((open_ch, _, _open_pos)) if open_ch == expected => {
                // Correct match
            }
            Some((open_ch, _, open_pos)) => {
                // Mismatched bracket
                self.errors.push(SyntaxError {
                    range: Range::new(current_pos, Position::new(current_pos.line, current_pos.character + 1)),
                    message: format!("Unclosed '{}' at line {}, expected a '{}' to close it",
                            open_ch, open_pos.line + 1, Self::closing_for(open_ch)),
                });
                // Put it back and try to match with earlier brackets
                self.stack.push((open_ch, 0, open_pos)); // byte pos not needed for error reporting
            }
            None => {
                // Extra closing bracket
                self.errors.push(SyntaxError {
                    range: Range::new(current_pos, Position::new(current_pos.line, current_pos.character + 1)),
                    message: format!("Unexpected '{}' - no matching opening bracket", ch),
                });
            }
        }
    }

    fn check_unclosed_first(&mut self) -> bool {
        if let Some((open_ch, _, open_pos)) = self.stack.pop() {
            self.errors.push(SyntaxError {
                range: Range::new(open_pos, Position::new(open_pos.line, open_pos.character + 1)),
                message: format!("Unclosed '{}' - expected '{}'", open_ch, Self::closing_for(open_ch)),
            });
            true // Found an error
        } else {
            false // No unclosed brackets
        }
    }

    fn closing_for(open_ch: char) -> char {
        match open_ch {
            '(' => ')',
            '[' => ']',
            '{' => '}',
            _ => open_ch, // fallback
        }
    }
}

impl Default for BracketChecker {
    fn default() -> Self {
        Self::new()
    }
}

// Helper to convert SyntaxError to LSP Diagnostic
impl From<SyntaxError> for Diagnostic {
    fn from(error: SyntaxError) -> Self {
        Diagnostic {
            range: error.range,
            severity: Some(DiagnosticSeverity::ERROR),
            message: error.message,
            source: Some("bracket-checker".to_string()),
            ..Default::default()
        }
    }
}
