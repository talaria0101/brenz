//! Code navigation related methods for Backend

use tower_lsp_server as tower_lsp;
use super::Backend;
use tower_lsp::lsp_types::*;
use tower_lsp::UriExt;
use tree_sitter::Node;
use crate::util;
use util::{logprint, LogType};

impl Backend {
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

    pub fn find_children_of_kind<'a>(&self, node: Node<'a>, kind: &str) -> Vec<Node<'a>>
    {
        let mut children: Vec<Node> = Vec::new();
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == kind {
                children.push(child);
            }
        }

        children
    }

    /**
     * Returns
     * - `path` of the script (if exists)
     * - `name` of the script
     * - `function` called or referenced
     *
     * of a `foreign_func_ref` or `foreign_call_expression` node
     **/
    pub(crate) fn process_foreign_fn(&self, node: Node, source: &str) -> Option<(Option<String>, String, String)>
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

    pub(crate) async fn resolve_target_fn(
        &self, start_node: Node<'_>, src: &str, current_uri: &Uri, comment: bool
    ) -> Option<(Location, Option<String>)>
    {
        if let Some(res) = self.resolve_foreign_target(start_node, src, comment).await {
            return Some(res);
        }

        if let Some(res) = self.resolve_local_target(start_node, src, current_uri, comment).await {
            return Some(res);
        }
        None
    }

    async fn resolve_foreign_target(
        &self, node: Node<'_>, src: &str, comment: bool
    ) -> Option<(Location, Option<String>)>
    {
        if let Some(foreign_ref) = self.find_child_of_kind(node, "foreign_function_ptr") {
            match self.process_foreign_fn(foreign_ref, src) {
                Some((p, s, f)) => {
                    logprint!(LogType::Info, "past processing: {:#?}, {}, {}", &p, &s, &f);
                    let root = self.workspace_root.lock().await;
                    match util::resolove_scr_path(&root, p, s) {
                        Ok(r) => {
                            logprint!(LogType::Info, "past resolove_path: {:#?}", &r);
                            let s_uri = Uri::from_file_path(r)?;
                            if let Some((rng, cmt)) = self.get_function(&s_uri, f, comment).await {
                                return Some((
                                    Location {
                                        uri: s_uri,
                                        range: rng,
                                    },
                                    cmt
                                ));
                            }
                        },
                        Err(e) => {
                            util::logprint!(LogType::Error, "Can't determine script path: {e}");
                            self.client.log_message(MessageType::ERROR, format!("Can't determine script path: {e}")).await;
                        }
                    };
                }
                None => {}
            };
        }
        None
    }

    async fn resolve_local_target(
        &self, node: Node<'_>, src: &str, current_uri: &Uri, comment: bool
    ) -> Option<(Location, Option<String>)>
    {
        let func = match self.find_child_of_kind(node, "local_function_ptr") {
            Some(child) => child.child_by_field_name("function"),
            None => node.child_by_field_name("function")
        };
        if let Some(func) = func {
            let fn_name = src[func.start_byte()..func.end_byte()].to_string();
            if let Some((rng, cmt)) = self.get_function(&current_uri, fn_name, comment).await {
                return Some((
                    Location {
                        uri: current_uri.clone(),
                        range: rng,
                    },
                    cmt
                ));
            }
        }
        None
    }

    pub(crate) fn find_var_def<'a>(&self, identifier: &str, current_node: Node<'a>, source: &str) -> Option<Node<'a>>
    {
        // Find containing function
        let function = self.find_parent_of_kind(current_node, "function_definition")?;

        let mut stop_at = current_node.start_byte();

        if let Some(assign_node) = self.find_parent_of_kind(current_node, "assignment_expression") {
            if let Some(assigned_value) = assign_node.child_by_field_name("assigned_value") {
                if assigned_value.byte_range().contains(&current_node.start_byte()) {
                    stop_at = assign_node.start_byte()
                }
            }
        };

        let mut last_assignment = None;

        // Walk function body looking for assignments before current position
        self.walk_tree_for_assignment(function, identifier, stop_at, source, &mut last_assignment);

        last_assignment
    }

    fn walk_tree_for_assignment<'a>(
        &self,
        node: Node<'a>,
        target_var: &str,
        stop_at: usize,
        source: &str,
        result: &mut Option<Node<'a>>,
    )
    {
        if node.start_byte() >= stop_at {
            return; // Past our position
        }

        if node.kind() == "assignment_expression" {
            if let Some(var_node) = node.child_by_field_name("variable") {
                let var_name = &source[var_node.start_byte()..var_node.end_byte()];
                if var_name == target_var {
                    *result = Some(node);
                }
            }
        }

        // Recurse into children
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.walk_tree_for_assignment(child, target_var, stop_at, source, result);
        }
    }
}
