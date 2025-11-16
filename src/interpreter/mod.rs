//! Script interpreter

use tower_lsp_server as tower_lsp;
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Range};
use tree_sitter::Tree;

#[derive(Debug, Clone)]
pub struct ScriptError
{
    pub range: Range,
    pub message: String,
}

pub struct Interpreter
{
    errors: Vec<ScriptError>,
}

impl Interpreter {
    pub fn _new() -> Self {
        Self {
            errors: Vec::new(),
        }
    }
    pub fn _interprete(_tree: &Tree) -> Vec<ScriptError>
    {
        Vec::new()
    }
}

// Helper to convert SyntaxError to LSP Diagnostic
impl From<ScriptError> for Diagnostic {
    fn from(error: ScriptError) -> Self {
        Diagnostic {
            range: error.range,
            severity: Some(DiagnosticSeverity::ERROR),
            message: error.message,
            source: Some("CoD GSC".to_string()),
            ..Default::default()
        }
    }
}
