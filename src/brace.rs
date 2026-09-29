//! Checks for mismatching brackets and quotes to give friendlier diagnostics.
//!
//! A single byte-offset scan that understands GSC strings and both
//! comment styles. Positions convert through the shared helper, so they
//! agree with the tree-sitter diagnostics.

use tower_lsp_server::lsp_types::{Diagnostic, DiagnosticSeverity, Range};

use crate::compiler::compile::byte_range_to_range;

#[derive(Debug, Clone)]
pub struct SyntaxError {
    pub range: Range,
    pub message: String,
}

/// Checks the script for mismatching brackets and quotes
///
/// TODO: Give this guy a better name, merge requests are welcome
pub struct BracketChecker {
    stack: Vec<(char, usize)>, // (bracket, byte offset)
    errors: Vec<SyntaxError>,
    src: String,
}

impl BracketChecker {
    /// Empty checker, ready to scan. Reused across files by clearing
    /// on every `check` call.
    pub fn new() -> Self {
        Self {
            stack: Vec::new(),
            errors: Vec::new(),
            src: String::new(),
        }
    }

    pub fn check(&mut self, source: &str) -> Vec<SyntaxError> {
        // One byte-offset pass over the file. Strings and comments
        // hide their brackets; everything unclosed gets reported.
        self.stack.clear();
        self.errors.clear();
        self.src = source.to_string();

        let bytes = source.as_bytes();
        let mut i = 0;
        let mut in_string = false;
        let mut string_start = 0;

        while i < bytes.len() {
            let ch = bytes[i] as char;
            // ASCII fast path: every structural character is ASCII, and
            // anything else cannot affect brackets, strings or comments.
            if !ch.is_ascii() {
                i += 1;
                while i < bytes.len() && !source.is_char_boundary(i) {
                    i += 1;
                }
                continue;
            }
            match ch {
                '\n' => {
                    // Strings cannot span lines in GSC.
                    if in_string {
                        self.error(
                            string_start,
                            string_start + 1,
                            "Unclosed string literal - strings cannot span multiple lines",
                        );
                        in_string = false;
                    }
                    i += 1;
                }
                '"' if in_string => {
                    in_string = false;
                    i += 1;
                }
                '"' => {
                    in_string = true;
                    string_start = i;
                    i += 1;
                }
                '\\' if in_string => {
                    // Skip the escaped character, whatever it is.
                    i += 1;
                    if i < bytes.len() {
                        i += 1;
                    }
                }
                '/' if !in_string && bytes.get(i + 1) == Some(&b'/') => {
                    // Line comment: skip to the newline.
                    i += 2;
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                }
                '/' if !in_string && bytes.get(i + 1) == Some(&b'*') => {
                    // Block comment: skip to `*/`, or report it.
                    let open = i;
                    i += 2;
                    let mut closed = false;
                    while i < bytes.len() {
                        if bytes[i] == b'\n' {
                            i += 1;
                        } else if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                            i += 2;
                            closed = true;
                            break;
                        } else {
                            i += 1;
                        }
                    }
                    if !closed {
                        self.error(open, open + 2, "Unclosed block comment - expected '*/'");
                    }
                }
                '(' | '[' | '{' if !in_string => {
                    self.stack.push((ch, i));
                    i += 1;
                }
                ')' | ']' | '}' if !in_string => {
                    self.check_closing_bracket(ch, i);
                    i += 1;
                }
                _ => {
                    i += 1;
                }
            }
        }

        // Every unclosed opener gets its own diagnostic, innermost first.
        while let Some((open_ch, open_at)) = self.stack.pop() {
            self.error(
                open_at,
                open_at + 1,
                &format!(
                    "Unclosed '{}' - expected '{}'",
                    open_ch,
                    Self::closing_for(open_ch)
                ),
            );
        }
        if in_string {
            self.error(string_start, bytes.len(), "Unclosed string literal");
        }

        std::mem::take(&mut self.errors)
    }

    fn error(&mut self, start: usize, end: usize, message: &str) {
        // Byte offsets convert to LSP ranges through the shared
        // helper, so these agree with the tree-sitter diagnostics.
        // Byte offsets come from scanning this exact source, so they
        // are always char boundaries except for pathological slicing,
        // which the helper clamps by construction.
        let end = end.min(self.src.len());
        self.errors.push(SyntaxError {
            range: byte_range_to_range(&self.src, start, end),
            message: message.to_string(),
        });
    }

    fn check_closing_bracket(&mut self, ch: char, pos: usize) {
        // A match pops quietly. A mismatch blames the closer and
        // keeps the opener around: it is usually still open. A lone
        // closer with an empty stack is simply unexpected.
        let expected = match ch {
            ')' => '(',
            ']' => '[',
            '}' => '{',
            _ => return,
        };

        match self.stack.pop() {
            Some((open_ch, _)) if open_ch == expected => {
                // Correct match.
            }
            Some((open_ch, open_at)) => {
                // Mismatched bracket: blame the closer, keep the opener
                // for a later match, as the opener is usually the one
                // that is still open.
                let line = self.src[..open_at].chars().filter(|c| *c == '\n').count() + 1;
                self.error(
                    pos,
                    pos + 1,
                    &format!(
                        "Unclosed '{}' at line {}, expected a '{}' to close it",
                        open_ch,
                        line,
                        Self::closing_for(open_ch)
                    ),
                );
                self.stack.push((open_ch, open_at));
            }
            None => {
                self.error(
                    pos,
                    pos + 1,
                    &format!("Unexpected '{}' - no matching opening bracket", ch),
                );
            }
        }
    }

    fn closing_for(open_ch: char) -> char {
        // The bracket that would have made this one happy.
        match open_ch {
            '(' => ')',
            '[' => ']',
            '{' => '}',
            _ => open_ch, // fallback
        }
    }

    /// Start position of a byte offset, for tests.
    #[cfg(test)]
    fn position_at(&self, byte: usize) -> (u32, u32) {
        let pos = byte_range_to_range(&self.src, byte, byte).start;
        (pos.line, pos.character)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn check(src: &str) -> Vec<String> {
        let mut checker = BracketChecker::new();
        checker.check(src).into_iter().map(|e| e.message).collect()
    }

    #[test]
    fn clean_file_is_quiet() {
        assert!(check("main()\n{\n\tx = ( 1 + [2] );\n\ts = \"a(b{c\";\n}\n").is_empty());
        assert!(check("").is_empty());
    }

    #[test]
    fn reports_every_unclosed_opener() {
        let errs = check("main(\n{\n\tif ( x \n");
        assert_eq!(errs.len(), 3, "{errs:?}");
        assert!(errs.iter().all(|m| m.starts_with("Unclosed")));
    }

    #[test]
    fn mismatch_keeps_opener() {
        // A typo'd closer reports the mismatch; the opener stays open
        // and is reported too if nothing closes it.
        let errs = check("main()\n{\n\tx = (];\n}\n");
        assert!(errs[0].contains("Unclosed '('"), "{errs:?}");
    }

    #[test]
    fn extra_closer() {
        let errs = check("main()\n{\n}\n}\n");
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].contains("Unexpected '}'"));
    }

    #[test]
    fn unclosed_block_comment() {
        // The swallowed `}` leaves `{` unclosed too: both are true.
        let errs = check("main()\n{\n\t/* comment\n\tx = 1;\n}\n");
        assert_eq!(errs.len(), 2, "{errs:?}");
        assert!(errs[0].contains("Unclosed block comment"));
    }

    #[test]
    fn brackets_in_strings_and_comments_ignored() {
        assert!(check("main() // ( [ {\n{\n\ts = \"}])\";\n}\n").is_empty());
        assert!(check("/* ( [ { */\nmain()\n{\n}\n").is_empty());
    }

    #[test]
    fn escaped_quote_stays_in_string() {
        assert!(check("main()\n{\n\ts = \"a\\\"(b\";\n}\n").is_empty());
    }

    #[test]
    fn unclosed_string_at_newline_and_eof() {
        let errs = check("main()\n{\n\ts = \"abc;\n}\n");
        assert!(
            errs.iter()
                .any(|m| m.contains("cannot span multiple lines")),
            "{errs:?}"
        );
        let errs = check("main()\n{\n\ts = \"abc;");
        assert!(
            errs.iter().any(|m| m == "Unclosed string literal"),
            "{errs:?}"
        );
    }

    #[test]
    fn positions_agree_with_source() {
        let mut checker = BracketChecker::new();
        let src = "main(\n{\n}\n";
        let errs = checker.check(src);
        assert_eq!(errs.len(), 1);
        // The unclosed `(` sits on line 0.
        assert_eq!(errs[0].range.start.line, 0);
        // ...and the helper maps bytes consistently.
        assert_eq!(checker.position_at(6), (1, 0));
    }
}
