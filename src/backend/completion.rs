//! Completion helpers: in-scope variables with inferred types,
//! fuzzy name matching, call snippets, and argument-position types.
//!
//! Types come from literals and declared builtin returns only. Anything
//! involving variables of unknown origin, user-function results, or the
//! game is `None` and simply skips type-based ranking, never filtering.

use std::collections::HashMap;
use tower_lsp_server::lsp_types::InsertTextFormat;
use tree_sitter::{Node, Tree};

use crate::doc::GscType;

/// A variable visible at the cursor, with a type when inferable.
#[derive(Debug, Clone)]
pub(crate) struct ScopeVar {
    pub name: String,
    pub ty: Option<GscType>,
}

/// Collect variables in scope at `byte`: the enclosing function's
/// parameters (unknown type) plus variables assigned before the cursor,
/// and top-level globals. Later writes overwrite earlier types.
pub(crate) fn scope_vars(tree: &Tree, src: &str, byte: usize) -> Vec<ScopeVar> {
    let mut order: Vec<String> = Vec::new();
    let mut types: HashMap<String, Option<GscType>> = HashMap::new();

    let root = tree.root_node();
    // Enclosing function, if any.
    let mut funcs: Vec<Node> = Vec::new();
    descendants_of_kind(root, "function_definition", &mut funcs);
    if let Some(func) = funcs
        .into_iter()
        .find(|n| n.start_byte() <= byte && byte <= n.end_byte())
    {
        let mut heads: Vec<Node> = Vec::new();
        descendants_of_kind(func, "func_head", &mut heads);
        for head in heads {
            let mut lists: Vec<Node> = Vec::new();
            descendants_of_kind(head, "parameter_list", &mut lists);
            for list in lists {
                let mut cursor = list.walk();
                for child in list.children(&mut cursor) {
                    if child.is_named() && child.kind() == "identifier" {
                        let name = slice(src, &child);
                        if !types.contains_key(&name) {
                            order.push(name.clone());
                        }
                        types.insert(name, None);
                    }
                }
            }
        }
        // Assignments in source order so identifier copies see types.
        let mut assigns: Vec<(usize, String, Node)> = Vec::new();
        let mut nodes: Vec<Node> = Vec::new();
        descendants_of_kind(func, "assignment_expression", &mut nodes);
        descendants_of_kind(func, "variable_declaration", &mut nodes);
        for node in nodes {
            let (target, value) = match node.kind() {
                "assignment_expression" => (
                    node.child_by_field_name("variable"),
                    node.child_by_field_name("assigned_value"),
                ),
                _ => {
                    let mut name = None;
                    let mut val = None;
                    let mut cursor = node.walk();
                    for child in node.children(&mut cursor) {
                        if !child.is_named() {
                            continue;
                        }
                        if child.kind() == "identifier" && name.is_none() {
                            name = Some(child);
                        } else {
                            val = Some(child);
                        }
                    }
                    (name, val)
                }
            };
            if let (Some(t), Some(v)) = (target, value) {
                // Fields wrap single expressions; see through them.
                let t = strip(t);
                if t.kind() == "identifier" {
                    assigns.push((t.start_byte(), slice(src, &t), strip(v)));
                }
            }
        }
        assigns.sort_by_key(|(at, _, _)| *at);
        for (at, name, rhs) in assigns {
            if at >= byte {
                continue;
            }
            // An unknown write never erases a known type: the last
            // thing provable wins, which is what ranking needs.
            let ty = infer_rhs(&rhs, src, &types);
            if ty.is_some() || !types.contains_key(&name) {
                if !types.contains_key(&name) {
                    order.push(name.clone());
                }
                types.insert(name, ty);
            }
        }
    }
    // Top-level globals are visible everywhere.
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if child.is_named() && child.kind() == "variable_declaration" {
            let mut name = None;
            let mut val = None;
            let mut inner = child.walk();
            for c in child.children(&mut inner) {
                if !c.is_named() {
                    continue;
                }
                if c.kind() == "identifier" && name.is_none() {
                    name = Some(slice(src, &c));
                } else {
                    val = Some(infer_rhs(&c, src, &types));
                }
            }
            if let Some(n) = name {
                if !types.contains_key(&n) {
                    order.push(n.clone());
                }
                types.insert(n, val.flatten());
            }
        }
    }

    order
        .into_iter()
        .map(|name| ScopeVar {
            ty: types.get(&name).cloned().flatten(),
            name,
        })
        .collect()
}

/// Type of an assigned value: literals, declared builtin returns, or a
/// previously typed variable. Everything else relies on runtime.
fn infer_rhs(node: &Node, src: &str, known: &HashMap<String, Option<GscType>>) -> Option<GscType> {
    infer_type(*node, src, &|name| known.get(name).cloned().flatten())
}

/// Infer an expression's type. The `lookup` resolves identifiers
/// against whatever scope the caller already collected.
pub(crate) fn infer_type(
    node: Node,
    src: &str,
    lookup: &dyn Fn(&str) -> Option<GscType>,
) -> Option<GscType> {
    let node = strip(node);
    match node.kind() {
        "number" => {
            let t = slice(src, &node);
            if t.contains('.') {
                Some(GscType::Float)
            } else {
                Some(GscType::Int)
            }
        }
        "string" => Some(GscType::String),
        "lstring" => Some(GscType::LString),
        "boolean" => Some(GscType::Bool),
        "vec3" => Some(GscType::Vector),
        "array" => Some(GscType::Array),
        "identifier" => lookup(&slice(src, &node)),
        "direct_call" => {
            let mut callee = None;
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.is_named() && child.kind() == "identifier" {
                    callee = Some(child);
                }
            }
            let name = slice(src, &callee?).to_lowercase();
            let sigs = crate::compiler::compile::builtin_sigs();
            let f = sigs.functions.get(&name)?;
            sig_return(&f.returns)
        }
        "object_call" => {
            let method = call_method(&node, src)?;
            let sigs = crate::compiler::compile::builtin_sigs();
            let m = sigs.methods.get(&method.to_lowercase())?;
            sig_return(&m.returns)
        }
        _ => None,
    }
}

/// A declared return becomes a variable type, except `any` (unknown)
// and object kinds without check value (kept, they still match exactly).
fn sig_return(returns: &Option<GscType>) -> Option<GscType> {
    match returns {
        None | Some(GscType::Any) => None,
        Some(t) => Some(t.clone()),
    }
}

fn strip(mut node: Node) -> Node {
    // Single-child wrappers the grammar leaves behind: an expression
    // in expression in call in call. Stops at real nodes, which
    // always carry two or more children (or none).
    loop {
        let kind = node.kind();
        if (kind == "expression" || kind == "vec1" || kind == "call_expression")
            && node.named_child_count() == 1
        {
            node = node.named_child(0).unwrap();
            continue;
        }
        return node;
    }
}

fn slice(src: &str, node: &Node) -> String {
    src[node.start_byte()..node.end_byte()].to_string()
}

/// Callee identifier of a direct or thread call, if syntactically there.
fn call_callee(node: &Node, src: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.is_named() && child.kind() == "identifier" {
            return Some(slice(src, &child));
        }
    }
    None
}

/// Method name of an object call: the identifier that is not the object.
fn call_method(node: &Node, src: &str) -> Option<String> {
    let mut seen_obj = false;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if !child.is_named() {
            continue;
        }
        match child.kind() {
            "identifier" => {
                if seen_obj {
                    return Some(slice(src, &child));
                }
                seen_obj = true;
            }
            "local_function_ptr" => {
                if let Some(f) = child.child_by_field_name("function") {
                    return Some(slice(src, &f));
                }
            }
            _ => {}
        }
    }
    None
}

/// The enclosing call at `byte`: lowercase callee, argument index, and
/// whether it is a method call. `None` on the callee name itself or
/// outside any call, where name completion applies instead.
pub(crate) fn enclosing_call(tree: &Tree, src: &str, byte: usize) -> Option<(String, usize, bool)> {
    let mut calls: Vec<Node> = Vec::new();
    for kind in ["direct_call", "thread_call", "object_call"] {
        descendants_of_kind(tree.root_node(), kind, &mut calls);
    }
    // Deepest call wins: nested calls complete their own arguments.
    calls.sort_by_key(|n| usize::MAX - (n.end_byte() - n.start_byte()));
    for call in calls {
        if !(call.start_byte() <= byte && byte <= call.end_byte()) {
            continue;
        }
        let is_method = call.kind() == "object_call";
        let name = if is_method {
            call_method(&call, src)?
        } else {
            call_callee(&call, src)?
        };
        // On the callee name itself: complete the name, not arguments.
        let mut cursor = call.walk();
        for child in call.children(&mut cursor) {
            if child.is_named()
                && child.kind() == "identifier"
                && child.start_byte() <= byte
                && byte <= child.end_byte()
                && slice(src, &child).eq_ignore_ascii_case(&name)
            {
                return None;
            }
        }
        let mut lists: Vec<Node> = Vec::new();
        descendants_of_kind(call, "argument_list", &mut lists);
        let Some(list) = lists
            .into_iter()
            .find(|l| l.start_byte() <= byte && byte <= l.end_byte())
        else {
            continue;
        };
        // Trailing comma selects the next, empty argument.
        return Some((
            name.to_lowercase(),
            arg_index_at(&list, src, byte),
            is_method,
        ));
    }
    None
}

/// Zero-based argument index at `byte`: expressions fully before the
/// cursor count, so a trailing comma selects the next slot.
fn arg_index_at(list: &Node, _src: &str, byte: usize) -> usize {
    let mut index = 0;
    let mut cursor = list.walk();
    for child in list.children(&mut cursor) {
        if !child.is_named() {
            continue;
        }
        if child.start_byte() > byte {
            break;
        }
        if child.end_byte() <= byte {
            index += 1;
        }
    }
    index
}

/// Match tiers. Prefix beats substring beats fuzzy wandering, and
/// everything stays below the next tier so ordering never mixes.
pub(crate) const SCORE_PREFIX: u32 = 1_000_000;
pub(crate) const SCORE_SUBSTRING: u32 = 500_000;
const SCORE_FUZZY_MAX: u32 = 499_999;
/// Width for descending score keys: `SCORE_SORT_MAX - score`.
pub(crate) const SCORE_SORT_MAX: u32 = 1_000_000;

/// Case-insensitive match quality: prefix, then substring, then a
/// nucleo subsequence score for typos. Empty prefix accepts
/// everything weakly. Non-ASCII falls back to plain matching,
/// since nucleo's ASCII view requires ASCII bytes.
pub(crate) fn fuzzy_score(prefix: &str, name: &str) -> u32 {
    if prefix.is_empty() {
        return 1;
    }
    // Fast paths stay exact and cheap: most keystrokes never reach
    // the matcher below.
    let lower = name.to_lowercase();
    let prefix = prefix.to_lowercase();
    if lower.starts_with(&prefix) {
        return SCORE_PREFIX;
    }
    if lower.contains(&prefix) {
        return SCORE_SUBSTRING;
    }
    // Subsequence typos go through nucleo, scaled to stay below a
    // plain substring hit. The matcher is built per call; the fast
    // paths above mean it only runs on the interesting tail.
    // (`prefer_prefix` would suit completion, but the config is
    // non-exhaustive; DEFAULT already ignores case.)
    if !prefix.is_ascii() || !name.is_ascii() {
        return 0;
    }
    let mut matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
    matcher
        .fuzzy_match(
            nucleo::Utf32Str::Ascii(name.as_bytes()),
            nucleo::Utf32Str::Ascii(prefix.as_bytes()),
        )
        .map(|s| (s as u32).min(SCORE_FUZZY_MAX))
        .unwrap_or(0)
}

/// `name(p1, p2)` for plain text, `name(${1:p1}, ${2:p2})` when the
/// client speaks snippets. Zero parameters complete to `name()`.
pub(crate) fn call_text(
    name: &str,
    params: &[String],
    snippet: bool,
) -> (String, Option<InsertTextFormat>) {
    if params.is_empty() {
        return (format!("{name}()"), None);
    }
    if !snippet {
        return (format!("{name}({})", params.join(", ")), None);
    }
    let parts: Vec<String> = params
        .iter()
        .enumerate()
        .map(|(i, p)| format!("${{{}:{}}}", i + 1, p))
        .collect();
    (
        format!("{name}({})", parts.join(", ")),
        Some(InsertTextFormat::SNIPPET),
    )
}

/// Parameter names from a `name( a, b )` head detail string.
pub(crate) fn head_params(detail: &str) -> Vec<String> {
    let Some(open) = detail.find('(') else {
        return Vec::new();
    };
    let Some(close) = detail.rfind(')') else {
        return Vec::new();
    };
    if close <= open {
        return Vec::new();
    }
    detail[open + 1..close]
        .split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

fn descendants_of_kind<'a>(node: Node<'a>, kind: &str, results: &mut Vec<Node<'a>>) {
    if node.kind() == kind {
        results.push(node);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        descendants_of_kind(child, kind, results);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Tree {
        let language = tree_sitter_gsc::LANGUAGE.into();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        parser.parse(src, None).unwrap()
    }

    #[test]
    fn fuzzy_ranks_prefix_over_substring_over_typo() {
        use super::{SCORE_PREFIX, SCORE_SUBSTRING};
        assert_eq!(fuzzy_score("", "anything"), 1);
        assert_eq!(fuzzy_score("get", "getEnt"), SCORE_PREFIX);
        assert_eq!(fuzzy_score("ent", "getEnt"), SCORE_SUBSTRING);
        assert_eq!(fuzzy_score("GET", "getEnt"), SCORE_PREFIX);
        assert_eq!(fuzzy_score("xyz", "getEnt"), 0);
        // Transposed letters still match, below any substring hit.
        let typo = fuzzy_score("gte", "getEnt");
        assert!(typo > 0 && typo < SCORE_SUBSTRING, "{typo}");
    }

    #[test]
    fn call_text_modes() {
        assert_eq!(call_text("f", &[], true), ("f()".to_string(), None));
        assert_eq!(
            call_text("f", &["a".to_string()], false),
            ("f(a)".to_string(), None)
        );
        assert_eq!(
            call_text("spawn", &["c".to_string(), "o".to_string()], true),
            (
                "spawn(${1:c}, ${2:o})".to_string(),
                Some(InsertTextFormat::SNIPPET)
            )
        );
    }

    #[test]
    fn head_params_parses() {
        assert_eq!(head_params("helper( a, b )"), vec!["a", "b"]);
        assert_eq!(head_params("main()"), Vec::<String>::new());
        assert_eq!(head_params("nope"), Vec::<String>::new());
    }

    #[test]
    fn scope_vars_types_and_order() {
        let src = "main()\n{\n\tn = 5;\n\tpos = ( 1, 2, 3 );\n\te = getent( \"a\", \"b\" );\n\ts = \"hi\";\n\tn = n + 1;\n\tx = n;\n}\n";
        let tree = parse(src);
        // After `x = ` (byte of `n;`): every variable with its type.
        let byte = src.find("x = n;").unwrap();
        let vars = scope_vars(&tree, src, byte);
        let get = |n: &str| vars.iter().find(|v| v.name == n).map(|v| v.ty.clone());
        assert_eq!(get("n"), Some(Some(GscType::Int)));
        assert_eq!(get("pos"), Some(Some(GscType::Vector)));
        assert_eq!(get("e"), Some(Some(GscType::Entity)));
        assert_eq!(get("s"), Some(Some(GscType::String)));
        // Before `n = 5`, `n` is invisible.
        let early = src.find("n = 5;").unwrap();
        let vars = scope_vars(&tree, src, early);
        assert!(vars.iter().all(|v| v.name != "n"));
    }

    #[test]
    fn enclosing_call_finds_index() {
        let src = "main()\n{\n\td = distance( pos, n );\n}\n";
        let tree = parse(src);
        // Inside the second argument.
        let byte = src.find("n );").unwrap();
        let found = enclosing_call(&tree, src, byte).unwrap();
        assert_eq!(found, ("distance".to_string(), 1, false));
        // On the callee name: name completion, not arguments.
        let byte = src.find("distance").unwrap() + 2;
        assert!(enclosing_call(&tree, src, byte).is_none());
    }
}
