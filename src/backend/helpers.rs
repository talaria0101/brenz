//! Helper methods for Backend

use tower_lsp_server as tower_lsp;

use super::Backend;
use tower_lsp::lsp_types::*;
use tree_sitter::{Node, Tree};

impl Backend {
    /// Helper to find node at position
    pub fn node_at_pos<'a>(&self, tree: &'a Tree, source: &str, position: Position) -> Option<Node<'a>>
    {
        let byte_offset = self.position_to_byte(source, position);
        let root = tree.root_node();

        root.descendant_for_byte_range(byte_offset, byte_offset)
    }

    pub(crate) fn byte_to_position(&self, source: &str, byte_offset: usize) -> Position
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

    pub(crate) fn position_to_byte(&self, source: &str, position: Position) -> usize
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

    pub(crate) async fn get_function(
        &self, uri: &Uri, name: String, comment: bool
    ) -> Option<(Range, Option<String>)>
    {
        let fn_defs = self.fn_defs.lock().await;
        let func = fn_defs.get(&uri)?;
        let rng = func.get(&name)?.0;
        let cmt = if comment {
            func.get(&name)?.1.clone()
        } else { None };

        Some((rng, cmt))
    }

    pub(crate) async fn hover_info(&self, data: (Location, Option<String>), current_uri: Uri) -> Option<String>
    {
        let location = data.0;
        let tsrc = {
            let dc = self.docs_content.lock().await;
            dc.get(&location.uri)?.clone()
        };

        let start = self.position_to_byte(&tsrc, location.range.start);
        let end = self.position_to_byte(&tsrc, location.range.end);
        let func = tsrc[start..end].to_string();

        let mut txt = String::new();
        txt.push_str(&format!("{func}\n___\n"));
        if location.uri != current_uri {
            txt.push_str(&format!("File: ``{}``\n___\n", location.uri.path().as_str()));
        }
        if let Some(cmt) = data.1 {
            txt.push_str(&format!("{}", cmt));
        }

        if txt.trim().len() != 0 {
            return Some(txt);
        }

        None
    }

    pub(crate) async fn get_syms(&self, uri: &Uri) -> Option<Vec<DocumentSymbol>>
    {
        let sym_defs = self.sym_defs.lock().await;
        match sym_defs.get(uri) {
            Some(defs) => Some(defs.clone()),
            None => None
        }
    }
}
