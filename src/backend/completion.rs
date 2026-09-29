use tower_lsp_server::lsp_types::{
    CompletionItem, CompletionItemKind, Documentation, MarkupContent, MarkupKind, Position,
};
use tree_sitter::Tree;

use crate::{backend::Backend, doc::GscType};

enum CompletionContext {
    GlobalScope,
    FunctionCall { function_name: String },
    StructMemberAccess { struct_name: String },
    VariableAssignment,
    FunctionDefinition,
    Unknown,
}

impl Backend {
    fn detect_completion_context(
        &self,
        tree: &Tree,
        source: &str,
        pos: Position,
    ) -> CompletionContext {
        let node = match self.node_at_pos(tree, source, pos) {
            Some(n) => n,
            None => return CompletionContext::GlobalScope,
        };

        if let Some(dot_node) = self.find_parent_of_kind(node, ".")
            && let Some(left_sib) = dot_node.prev_sibling()
        {
            let struct_name = &source[left_sib.start_byte()..left_sib.end_byte()];
            return CompletionContext::StructMemberAccess {
                struct_name: struct_name.to_string(),
            };
        }

        if let Some(call_node) = self
            .find_parent_of_kind(node, "direct_call")
            .or_else(|| self.find_parent_of_kind(node, "thread_call"))
            .or_else(|| self.find_parent_of_kind(node, "object_call"))
            && let Some(func_name_node) = call_node.child_by_field_name("function")
        {
            let function_name = &source[func_name_node.start_byte()..func_name_node.end_byte()];
            return CompletionContext::FunctionCall {
                function_name: function_name.to_string(),
            };
        }

        if self
            .find_parent_of_kind(node, "assignment_expression")
            .is_some()
        {
            return CompletionContext::VariableAssignment;
        }

        CompletionContext::GlobalScope
    }

    async fn get_struct_member_completions(
        &self,
        struct_type: &str,
        prefix: &str,
    ) -> Vec<CompletionItem> {
        let mut suggestions = Vec::new();

        // Check builtin structs first
        // let builtins = self.builtins_doc.lock().await;

        // For example, if struct_type is "level", "self", "game", etc.
        match struct_type.to_lowercase().as_str() {
            "level" => {
                // Add level-specific members
                suggestions.push(CompletionItem {
                    label: "flag".to_string(),
                    kind: Some(CompletionItemKind::FIELD),
                    detail: Some("level.flag".to_string()),
                    documentation: Some(Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: "Level flag status".to_string(),
                    })),
                    ..Default::default()
                });
                // Add more level members...
            }
            "self" | "player" => {
                // Add player/entity members
                suggestions.push(CompletionItem {
                    label: "health".to_string(),
                    kind: Some(CompletionItemKind::FIELD),
                    detail: Some("self.health".to_string()),
                    ..Default::default()
                });
                // Add more player members...
            }
            "game" => {
                // Add game-specific members
                suggestions.push(CompletionItem {
                    label: "type".to_string(),
                    kind: Some(CompletionItemKind::FIELD),
                    detail: Some("game[type]".to_string()),
                    ..Default::default()
                });
                // Add more game members...
            }
            _ => {
                // Check if it's a user-defined struct
                // This would require struct definition parsing
            }
        }

        // Filter by prefix
        suggestions.retain(|item| item.label.starts_with(prefix));

        suggestions
    }

    async fn get_function_call_completions(
        &self,
        tree: &Tree,
        source: &str,
        pos: Position,
        function_name: &str,
        prefix: &str,
    ) -> Vec<CompletionItem> {
        let mut suggestions = Vec::new();
        let Some(_node) = self.node_at_pos(tree, source, pos) else {
            return Vec::new();
        };

        // Check builtin functions
        let builtins = self.builtins_doc.lock().await;

        if let Some(builtin_fn) = builtins.functions.get(&function_name.to_lowercase()) {
            // Add parameter names as completion suggestions
            for param in &builtin_fn.params {
                suggestions.push(CompletionItem {
                    label: param.name.clone(),
                    kind: Some(CompletionItemKind::VARIABLE),
                    detail: Some(format!(
                        "{} ({})",
                        param.name,
                        match param.ptype {
                            GscType::Int => "int",
                            GscType::Float => "float",
                            GscType::String => "string",
                            // ... other types
                            _ => "any",
                        }
                    )),
                    ..Default::default()
                });
            }
        }

        // Filter by prefix
        suggestions.retain(|item| item.label.starts_with(prefix));

        suggestions
    }
}
