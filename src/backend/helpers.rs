//! Helper methods for Backend

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
}
