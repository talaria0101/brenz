//! Code navigation related methods for Backend

use super::Backend;
use tower_lsp::lsp_types::*;
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

    pub(crate) async fn resolve_call_target(
        &self, call_node: Node<'_>, src: &str, current_uri: &Url
    ) ->Option<Location>
    {
        if let Some(location) = self.resolve_foreign_target(call_node, src).await {
            return Some(location);
        }

        if let Some(location) = self.resolve_local_target(call_node, src, current_uri).await {
            return Some(location);
        }
        None
    }

    async fn resolve_foreign_target(&self, call_node: Node<'_>, src: &str) -> Option<Location>
    {
        if let Some(foreign_ref) = self.find_child_of_kind(call_node, "foreign_function_ptr") {
            match self.process_foreign_fn(foreign_ref, src) {
                Some((p, s, f)) => {
                    logprint!(LogType::Info, "past processing: {:#?}, {}, {}", &p, &s, &f);
                    let root = self.workspace_root.lock().await;
                    match util::resolove_scr_path(&root, p, s) {
                        Ok(r) => {
                            logprint!(LogType::Info, "past resolove_path: {:#?}", &r);
                            let s_uri = Url::from_file_path(r).unwrap();
                            let fn_defs = self.fn_defs.lock().await;
                            let func = fn_defs.get(&s_uri)?;
                            return Some(Location {
                                uri: s_uri,
                                range: *func.get(&f)?,
                            });
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
        &self, call_node: Node<'_>, src: &str, current_uri: &Url
    ) -> Option<Location>
    {
        if let Some(func) = call_node.child_by_field_name("function") {
            let fn_name = &src[func.start_byte()..func.end_byte()];
            let fn_defs = self.fn_defs.lock().await;
            let func = fn_defs.get(&current_uri)?;
            return Some(Location {
                uri: current_uri.clone(),
                range: *func.get(fn_name)?,
            });
        }
        None
    }
}
