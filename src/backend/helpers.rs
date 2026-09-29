//! Helper methods for Backend

use tower_lsp_server as tower_lsp;

use crate::doc::ScriptCallable;

use super::Backend;
use tower_lsp::lsp_types::*;
use tree_sitter::{Node, Tree};

impl Backend {
    /// Helper to find node at position
    pub fn node_at_pos<'a>(&self, tree: &'a Tree, source: &str, position: Position) -> Option<Node<'a>>
    {
        let byte_offset = Self::position_to_byte(source, position);
        let root = tree.root_node();

        root.descendant_for_byte_range(byte_offset, byte_offset)
    }

    // Helper to extract the word being typed at cursor position
    pub fn get_word_at_position<'a>(&self, source: &'a str, position: Position) -> &'a str {
        let lines: Vec<&str> = source.lines().collect();
        if position.line as usize >= lines.len() {
            return "";
        }

        let line = lines[position.line as usize];
        let col = position.character as usize;

        if col > line.len() {
            return "";
        }

        // Find the start of the word (walk backwards)
        let mut start = col;
        while start > 0 {
            let ch = line.chars().nth(start - 1).unwrap();
            if !ch.is_alphanumeric() && ch != '_' {
                break;
            }
            start -= 1;
        }

        // Find the end of the word (walk forwards)
        let mut end = col;
        while end < line.len() {
            let ch = line.chars().nth(end).unwrap();
            if !ch.is_alphanumeric() && ch != '_' {
                break;
            }
            end += 1;
        }

        &line[start..end]
    }

    pub(crate) fn byte_to_position(source: &str, byte_offset: usize) -> Position
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

    pub(crate) fn position_to_byte(source: &str, position: Position) -> usize
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
        // The engine interns names lowercased, so `Foo` and `foo` are
        // the same function. Prefer the exact spelling, then fall back
        // to a case-insensitive scan.
        let fn_defs = self.fn_defs.lock().await;
        let func = fn_defs.get(uri)?;
        let hit = func.get(&name).or_else(|| {
            let lower = name.to_lowercase();
            func.iter().find(|(k, _)| k.to_lowercase() == lower).map(|(_, v)| v)
        })?;
        let cmt = if comment { hit.1.clone() } else { None };

        Some((hit.0, cmt))
    }

    pub(crate) fn info_from_builtin<T: ScriptCallable>(builtin: &T, sign: &str) -> String
    {
        let info = builtin.info();
        let called_on = builtin.called_on();
        let example = builtin.example();
        let mut txt = String::new();
        txt.push_str(&format!("## `{}`\n", sign));
        if called_on.is_empty() {
            txt.push_str("___\n");
        }
        else {
            txt.push_str(&format!("Called on: `{called_on}`\n___\n"));
        }
        txt.push_str("Builtin Method\n___\n");
        txt.push_str(&format!("{}\n___\nExample:\n", &info));
        txt.push_str(&format!("```gsc\n{}\n```", &example));
        txt
    }

    pub(crate) async fn hover_info(
        &self, data: (Location, Option<String>), current_uri: Uri
    ) -> Option<String>
    {
        let location = data.0;
        let tsrc = {
            let dc = self.docs_content.lock().await;
            dc.get(&location.uri)?.clone()
        };

        let start = Self::position_to_byte(&tsrc, location.range.start);
        let end = Self::position_to_byte(&tsrc, location.range.end);
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

    pub(crate) fn comp_item_for_builtin<T: ScriptCallable>(name: &str, builtin: &T) -> CompletionItem
    {
        let camel_name = builtin.camel_name().unwrap_or(name.to_string());
        let sign = builtin.sign();
        let params = builtin.param_names();
        let kind = builtin.kind();

        let item_kind = match kind.as_str() {
            "Builtin Function" => CompletionItemKind::FUNCTION,
            "Builtin Method" => CompletionItemKind::METHOD,
            _ => CompletionItemKind::KEYWORD
        };

        let doc_string = Self::info_from_builtin(builtin, &sign); // reuse sign
        let doc = Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: doc_string
        });

        let mut inset_text = String::new();
        inset_text.push_str(&camel_name);
        inset_text.push('(');
        inset_text.push_str(&params.join(", "));
        inset_text.push(')');

        CompletionItem {
            label: camel_name,
            detail: Some(sign),
            documentation: Some(doc),
            kind: Some(item_kind),
            insert_text: Some(inset_text),
            ..Default::default()
        }
    }
}
