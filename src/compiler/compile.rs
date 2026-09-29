//! Bytecode compiler: tree-sitter GSC AST to [`Program`].
//!
//! The pipeline mirrors coduomp's (`script_compile_statements.c` /
//! `script_compile_expr.c` feeding an emitter): statements lower to
//! jumps, expressions to stack code, calls to named invocations resolved
//! at runtime so cross-file references still compile.
//!
//! Deviations from the engine, all documented at the use site:
//! - Function names resolve case-insensitively (lowercased).
//! - Missing call arguments arrive as `undefined`; extras are dropped.
//! - Engine-only operations (`wait`, `thread`, entities, foreign calls)
//!   compile to `NeedGame` and fail only if executed.
//! - Compound assignment and `++`/`--` on complex targets evaluate the
//!   base expression once, via hidden frame slots.
//! - Reading an undeclared variable yields `undefined` (which is why
//!   `isdefined` exists), it never traps.

use std::collections::HashMap;
use tree_sitter::Node;

use super::builtin;
use super::op::{Instr, Op, Operand};
use super::value::{is_zero_literal, Value};
use super::ScriptError;
use crate::doc::{Builtins, GscType};
use std::sync::OnceLock;

/// A compiled script: shared code plus the function table.
#[derive(Debug, Clone, Default)]
pub(crate) struct Program {
    pub code: Vec<Instr>,
    pub functions: HashMap<String, FuncInfo>,
    /// Number of global slots (the `__globals` frame size).
    pub global_slots: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct FuncInfo {
    pub entry: usize,
    pub params: Vec<String>,
    /// Total frame slots (params + locals + temps).
    pub slots: usize,
}

/// Compile a parsed tree. `src` is the original source for spans.
pub(crate) fn compile(tree: &tree_sitter::Tree, src: &str) -> Result<Program, Vec<ScriptError>> {
    Compiler::new(src).compile_program(tree)
}

/// Engine method calls need a game; anything else called on an object
/// is a local `obj method()` call. Names come from `builtins.ron`.
fn is_builtin_method(name: &str) -> bool {
    builtin::is_engine_method(name)
}

/// Parsed builtin signatures, for static type checks.
fn builtin_sigs() -> &'static Builtins {
    static SIGS: OnceLock<Builtins> = OnceLock::new();
    SIGS.get_or_init(|| {
        ron::from_str(include_str!("../assets/builtins.ron")).expect("builtins.ron")
    })
}

/// Static representative for a declared return type. `None` means
/// unknown: `any`, or object kinds without useful check semantics.
fn return_rep(t: &GscType) -> Option<Value> {
    use crate::doc::GscType::*;
    match t {
        Bool => Some(Value::Int(0)),
        Int => Some(Value::Int(0)),
        Float => Some(Value::Float(0.0)),
        String => Some(Value::Str(::std::string::String::new())),
        LString => Some(Value::IStr(::std::string::String::new())),
        Array => Some(Value::Array(std::rc::Rc::new(std::cell::RefCell::new(Vec::new())))),
        Vector => Some(Value::Vec3([0.0; 3])),
        Entity => Some(Value::Entity),
        Any | HudElem | Struct => None,
    }
}

/// Leading-numeric text, mirroring the engine's `atoi`/`atof` leniency:
/// an optional sign followed by a digit (or `.` then a digit).
fn starts_numeric(s: &str) -> bool {
    let t = s.trim();
    let t = t.strip_prefix(['+', '-']).unwrap_or(t);
    let mut chars = t.chars();
    match chars.next() {
        Some(c) if c.is_ascii_digit() => true,
        Some('.') => chars.next().is_some_and(|c| c.is_ascii_digit()),
        _ => false,
    }
}

/// `atoi`-legal text under `CastInt`: a parsed integer, leading digits,
/// or an actual zero spelling.
fn castable_int_str(s: &str) -> bool {
    let t = s.trim();
    t.parse::<i32>().is_ok() || starts_numeric(t) || is_zero_literal(t)
}

/// Certain argument mismatch against a declared parameter type.
/// Returns the message, or `None` when legal or unknowable. Localized
/// strings and local structs take unclear engine paths and are never
/// flagged, nor is anything against `any`.
fn arg_mismatch(expected: &GscType, actual: &Value, fname: &str, pname: &str) -> Option<String> {
    use crate::doc::GscType::*;
    if matches!(actual, Value::IStr(_) | Value::Struct(_)) {
        return None;
    }
    let ok = match expected {
        Any | HudElem | Struct | LString => true,
        Bool => matches!(actual, Value::Int(_) | Value::Float(_))
            || matches!(actual, Value::Str(s) if castable_int_str(s)),
        Int => matches!(actual, Value::Int(_) | Value::Float(_))
            || matches!(actual, Value::Str(s) if castable_int_str(s)),
        Float => {
            matches!(actual, Value::Int(_) | Value::Float(_))
                || matches!(actual, Value::Str(s) if starts_numeric(s))
        }
        // `CastString` converts ints, floats and vectors.
        String => matches!(actual, Value::Str(_) | Value::Int(_) | Value::Float(_) | Value::Vec3(_)),
        // `CastVector` accepts vectors only.
        Vector => matches!(actual, Value::Vec3(_)),
        Array => matches!(actual, Value::Array(_)),
        Entity => matches!(actual, Value::Entity),
    };
    if ok {
        return None;
    }
    let want = format!("{expected:?}").to_lowercase();
    Some(format!(
        "{fname} expects {want} for '{pname}', got {}",
        actual.type_name()
    ))
}

struct LoopCtx {
    break_patches: Vec<usize>,
    /// Direct continue target, when known (while, `for(;;)`, foreach).
    continue_target: Option<usize>,
    /// Placeholder continues patched once the target is emitted.
    continue_patches: Vec<usize>,
    /// `switch` forwards `continue` to the enclosing loop.
    is_switch: bool,
}

struct FnCtx {
    locals: Vec<String>,
    loops: Vec<LoopCtx>,
}

struct Compiler<'a> {
    src: &'a str,
    code: Vec<Instr>,
    functions: HashMap<String, FuncInfo>,
    errors: Vec<ScriptError>,
    cur: Option<FnCtx>,
    globals: Vec<String>,
    temps: u32,
}

impl<'a> Compiler<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src,
            code: Vec::new(),
            functions: HashMap::new(),
            errors: Vec::new(),
            cur: None,
            globals: Vec::new(),
            temps: 0,
        }
    }

    fn text(&self, node: &Node<'a>) -> &'a str {
        &self.src[node.start_byte()..node.end_byte()]
    }

    fn fail(&mut self, node: &Node, message: String) {
        self.errors.push(ScriptError {
            range: byte_range_to_range(self.src, node.start_byte(), node.end_byte()),
            message,
        });
    }

    fn emit(&mut self, op: Op, arg: Operand, node: &Node<'a>) {
        self.code.push(Instr::new(op, arg, node.start_byte()));
    }

    fn emit_simple(&mut self, op: Op, node: &Node<'a>) {
        self.emit(op, Operand::None, node);
    }

    /// Placeholder jump patched later. Returns the instruction index.
    fn emit_jump(&mut self, op: Op, node: &Node<'a>) -> usize {
        let at = self.code.len();
        self.emit(op, Operand::Addr(usize::MAX), node);
        at
    }

    fn patch(&mut self, at: usize) {
        let target = self.code.len();
        match &mut self.code[at].arg {
            Operand::Addr(a) => *a = target,
            _ => panic!("patch of non-jump"),
        }
    }

    fn patch_to(&mut self, at: usize, target: usize) {
        match &mut self.code[at].arg {
            Operand::Addr(a) => *a = target,
            _ => panic!("patch of non-jump"),
        }
    }

    /// Strip single-child wrapper rules (`expression`, `statement`,
    /// `call_expression`) the grammar leaves in the tree.
    fn peel(&self, mut node: Node<'a>) -> Node<'a> {
        loop {
            let kind = node.kind();
            if (kind == "expression" || kind == "statement" || kind == "call_expression")
                && node.named_child_count() == 1
            {
                node = node.named_child(0).unwrap();
                continue;
            }
            return node;
        }
    }

    fn named_children(&self, node: &Node<'a>) -> Vec<Node<'a>> {
        let mut out = Vec::new();
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.is_named() {
                out.push(child);
            }
        }
        out
    }

    fn compile_program(mut self, tree: &'a tree_sitter::Tree) -> Result<Program, Vec<ScriptError>> {
        let root = tree.root_node();
        // Pre-register every function so forward references compile.
        let mut defs: Vec<(String, Vec<String>, Node)> = Vec::new();
        for child in self.named_children(&root) {
            if child.kind() != "function_definition" {
                continue;
            }
            let Some(head) = child.child_by_field_name("func_head") else {
                continue;
            };
            let Some(name_node) = head.named_child(0) else {
                continue;
            };
            let name = self.text(&name_node).to_lowercase();
            let mut params = Vec::new();
            for h in self.named_children(&head) {
                if h.kind() == "parameter_list" {
                    for p in self.named_children(&h) {
                        if p.kind() == "identifier" {
                            params.push(self.text(&p).to_string());
                        }
                    }
                }
            }
            defs.push((name, params, child));
        }
        for (name, params, _) in &defs {
            self.functions.entry(name.clone()).or_insert(FuncInfo {
                entry: usize::MAX,
                params: params.clone(),
                slots: 0,
            });
        }
        for (name, _, node) in &defs {
            self.compile_function(name, node);
        }
        // Top-level variable declarations run once as `__globals`.
        let entry = self.code.len();
        self.cur = Some(FnCtx {
            locals: Vec::new(),
            loops: Vec::new(),
        });
        for child in self.named_children(&root) {
            if child.kind() == "variable_declaration" {
                self.compile_global_decl(&child);
            }
        }
        self.emit_simple(Op::Return, &root);
        let global_slots = self.cur.take().map(|c| c.locals.len()).unwrap_or(0);
        if !self.errors.is_empty() {
            return Err(self.errors);
        }
        let mut program = Program {
            code: std::mem::take(&mut self.code),
            functions: std::mem::take(&mut self.functions),
            global_slots,
        };
        program.functions.insert(
            "__globals".to_string(),
            FuncInfo {
                entry,
                params: Vec::new(),
                slots: global_slots,
            },
        );
        Ok(program)
    }

    fn compile_global_decl(&mut self, node: &Node<'a>) {
        let mut name = None;
        let mut value = None;
        for child in self.named_children(node) {
            let child = self.peel(child);
            if child.kind() == "identifier" && name.is_none() {
                name = Some(self.text(&child).to_string());
            } else if is_value_kind(child.kind()) {
                value = Some(child);
            }
        }
        let (Some(n), Some(v)) = (name, value) else {
            self.fail(node, "bad variable declaration".to_string());
            return;
        };
        let idx = self.global_slot(&n);
        self.compile_expr(&v);
        self.emit(Op::SetGlobal, Operand::Slot(idx), node);
    }

    fn global_slot(&mut self, name: &str) -> u32 {
        if let Some(i) = self.globals.iter().position(|g| g == name) {
            return i as u32;
        }
        self.globals.push(name.to_string());
        (self.globals.len() - 1) as u32
    }

    // -- locals ----------------------------------------------------

    fn local_slot(&mut self, name: &str) -> u32 {
        let cur = self.cur.as_mut().expect("no function");
        if let Some(i) = cur.locals.iter().position(|l| l == name) {
            return i as u32;
        }
        cur.locals.push(name.to_string());
        (cur.locals.len() - 1) as u32
    }

    /// Hidden frame slot for address temporaries. `$` cannot appear in
    /// GSC identifiers, so these never collide with user variables.
    fn temp_slot(&mut self) -> u32 {
        let name = format!("$t{}", self.temps);
        self.temps += 1;
        self.local_slot(&name)
    }

    /// Resolve a variable name: locals first, then globals.
    fn resolve(&mut self, name: &str) -> VarRef {
        if let Some(cur) = self.cur.as_ref() {
            if let Some(i) = cur.locals.iter().position(|l| l == name) {
                return VarRef::Local(i as u32);
            }
        }
        if let Some(i) = self.globals.iter().position(|g| g == name) {
            return VarRef::Global(i as u32);
        }
        VarRef::NewLocal(name.to_string())
    }

    fn emit_get_var(&mut self, r: &VarRef, node: &Node<'a>) {
        match r.clone() {
            VarRef::Local(i) => {
                self.emit(Op::GetLocal, Operand::Slot(i), node);
            }
            VarRef::Global(i) => {
                self.emit(Op::GetGlobal, Operand::Slot(i), node);
            }
            VarRef::NewLocal(_) => {
                self.emit_simple(Op::GetUndefined, node);
            }
        };
    }

    /// Store the stack top into a variable, leaving the value behind so
    /// assignments yield their value.
    fn emit_set_var(&mut self, name: &str, node: &Node<'a>) {
        self.emit_simple(Op::Dup, node);
        match self.resolve(name) {
            VarRef::Local(i) => {
                self.emit(Op::SetLocal, Operand::Slot(i), node);
            }
            VarRef::Global(i) => {
                self.emit(Op::SetGlobal, Operand::Slot(i), node);
            }
            VarRef::NewLocal(n) => {
                let i = self.local_slot(&n);
                self.emit(Op::SetLocal, Operand::Slot(i), node);
            }
        }
    }

    // -- functions -------------------------------------------------

    fn compile_function(&mut self, name: &str, node: &Node<'a>) {
        let entry = self.code.len();
        if let Some(info) = self.functions.get_mut(name) {
            info.entry = entry;
            let params = info.params.clone();
            self.cur = Some(FnCtx {
                locals: params,
                loops: Vec::new(),
            });
        } else {
            return;
        }
        if let Some(block) = node.child_by_field_name("func_block") {
            self.compile_block(&block);
        }
        self.emit_simple(Op::GetUndefined, node);
        self.emit_simple(Op::Return, node);
        if let Some(cur) = self.cur.take() {
            if let Some(info) = self.functions.get_mut(name) {
                info.slots = cur.locals.len();
            }
        }
    }

    // -- statements --------------------------------------------------

    fn compile_block(&mut self, node: &Node<'a>) {
        // No early exit on errors: a language server reports every
        // problem at once, and `None`-poisoned static values already
        // suppress cascading follow-ups.
        for child in self.named_children(node) {
            let stmt = self.peel(child);
            self.compile_stmt(&stmt);
        }
    }

    fn compile_stmt(&mut self, node: &Node<'a>) {
        match node.kind() {
            "expression_statement" => {
                for child in self.named_children(node) {
                    let expr = self.peel(child);
                    self.compile_expr(&expr);
                    self.emit_simple(Op::DropTop, node);
                }
            }
            "variable_declaration" => {
                let mut name = None;
                let mut value = None;
                for child in self.named_children(node) {
                    let child = self.peel(child);
                    if child.kind() == "identifier" && name.is_none() {
                        name = Some(self.text(&child).to_string());
                    } else if is_value_kind(child.kind()) {
                        value = Some(child);
                    }
                }
                let (Some(n), Some(v)) = (name, value) else {
                    self.fail(node, "bad variable declaration".to_string());
                    return;
                };
                self.compile_expr(&v);
                // Locals only: declarations never touch globals.
                let i = self.local_slot(&n);
                self.emit(Op::SetLocal, Operand::Slot(i), node);
            }
            "block" => self.compile_block(node),
            "if_statement" => {
                let cond = node.child_by_field_name("condition");
                let cons = node.child_by_field_name("consequence");
                let alt = node.child_by_field_name("alternative");
                let (Some(cond), Some(cons)) = (cond, cons) else {
                    self.fail(node, "bad if statement".to_string());
                    return;
                };
                let cond = self.peel(cond);
                if let Some(v) = self.static_value(&cond) {
                    self.check_truthy(&v, &cond);
                }
                self.compile_expr(&cond);
                let else_jump = self.emit_jump(Op::JumpOnFalse, node);
                let cons = self.peel(cons);
                self.compile_stmt(&cons);
                if let Some(alt) = alt {
                    let end_jump = self.emit_jump(Op::Jump, node);
                    self.patch(else_jump);
                    let alt = self.peel(alt);
                    self.compile_stmt(&alt);
                    self.patch(end_jump);
                } else {
                    self.patch(else_jump);
                }
            }
            "while_statement" => {
                let top = self.code.len();
                let cond = node.child_by_field_name("loop_condition");
                let body = node.child_by_field_name("loop_block");
                let (Some(cond), Some(body)) = (cond, body) else {
                    self.fail(node, "bad while statement".to_string());
                    return;
                };
                let cond = self.peel(cond);
                if let Some(v) = self.static_value(&cond) {
                    self.check_truthy(&v, &cond);
                }
                self.compile_expr(&cond);
                let end_jump = self.emit_jump(Op::JumpOnFalse, node);
                self.cur.as_mut().expect("no function").loops.push(LoopCtx {
                    break_patches: Vec::new(),
                    continue_target: Some(top),
                    continue_patches: Vec::new(),
                    is_switch: false,
                });
                let body = self.peel(body);
                self.compile_stmt(&body);
                self.emit(Op::JumpBack, Operand::Addr(top), node);
                self.patch(end_jump);
                self.close_loop(self.code.len());
            }
            "for_statement" => self.compile_for(node),
            "foreach_statement" => self.compile_foreach(node),
            "switch_statement" => self.compile_switch(node),
            "return_statement" => {
                if let Some(ret) = node.child_by_field_name("returned") {
                    let ret = self.peel(ret);
                    self.compile_expr(&ret);
                } else {
                    self.emit_simple(Op::GetUndefined, node);
                }
                self.emit_simple(Op::Return, node);
            }
            "break_statement" => {
                if self.cur.as_ref().expect("no function").loops.is_empty() {
                    self.fail(node, "break outside a loop".to_string());
                } else {
                    let patch = self.emit_jump(Op::Jump, node);
                    self.cur
                        .as_mut()
                        .expect("no function")
                        .loops
                        .last_mut()
                        .expect("loop")
                        .break_patches
                        .push(patch);
                }
            }
            "continue_statement" => self.compile_continue(node),
            "wait_statement" => {
                self.emit(
                    Op::NeedGame,
                    Operand::Name("wait needs a game scheduler".to_string()),
                    node,
                );
                // Keep the stack balanced for code after the wait.
                self.emit_simple(Op::GetUndefined, node);
                self.emit_simple(Op::DropTop, node);
            }
            _ => self.fail(
                node,
                format!("cannot compile {} statement", node.kind()),
            ),
        }
    }

    fn compile_continue(&mut self, node: &Node<'a>) {
        enum Action {
            Direct(usize),
            Patch,
            None,
        }
        let action = {
            let mut found = Action::None;
            for ctx in self.cur.as_ref().expect("no function").loops.iter().rev() {
                if ctx.is_switch {
                    continue;
                }
                found = match ctx.continue_target {
                    Some(target) => Action::Direct(target),
                    None => Action::Patch,
                };
                break;
            }
            found
        };
        match action {
            Action::Direct(target) => {
                self.emit(Op::JumpBack, Operand::Addr(target), node);
            }
            Action::Patch => {
                let patch = self.emit_jump(Op::Jump, node);
                for ctx in self
                    .cur
                    .as_mut()
                    .expect("no function")
                    .loops
                    .iter_mut()
                    .rev()
                {
                    if ctx.is_switch {
                        continue;
                    }
                    ctx.continue_patches.push(patch);
                    break;
                }
            }
            Action::None => self.fail(node, "continue outside a loop".to_string()),
        }
    }

    fn close_loop(&mut self, end: usize) {
        let ctx = self.cur.as_mut().expect("no function").loops.pop().unwrap();
        for p in ctx.break_patches {
            self.patch_to(p, end);
        }
        for p in ctx.continue_patches {
            self.patch_to(p, end);
        }
    }

    fn compile_for(&mut self, node: &Node<'a>) {
        let cond_node = node.child_by_field_name("loop_condition");
        let body = node.child_by_field_name("loop_block");
        let (Some(cond_node), Some(body)) = (cond_node, body) else {
            self.fail(node, "bad for statement".to_string());
            return;
        };
        if cond_node.kind() == "infinite_for" {
            let top = self.code.len();
            self.cur.as_mut().expect("no function").loops.push(LoopCtx {
                break_patches: Vec::new(),
                continue_target: Some(top),
                continue_patches: Vec::new(),
                is_switch: false,
            });
            let body = self.peel(body);
            self.compile_stmt(&body);
            self.emit(Op::JumpBack, Operand::Addr(top), node);
            self.close_loop(self.code.len());
            return;
        }
        // finite_for: init; condition; update.
        let init = cond_node.child_by_field_name("init");
        let condition = cond_node.child_by_field_name("condition");
        let update = cond_node.child_by_field_name("update");
        let (Some(init), Some(condition), Some(update)) = (init, condition, update) else {
            self.fail(node, "bad for statement".to_string());
            return;
        };
        let init = self.peel(init);
        self.compile_expr(&init);
        self.emit_simple(Op::DropTop, node);
        let top = self.code.len();
        let condition = self.peel(condition);
        if let Some(v) = self.static_value(&condition) {
            self.check_truthy(&v, &condition);
        }
        self.compile_expr(&condition);
        let end_jump = self.emit_jump(Op::JumpOnFalse, node);
        self.cur.as_mut().expect("no function").loops.push(LoopCtx {
            break_patches: Vec::new(),
            continue_target: None,
            continue_patches: Vec::new(),
            is_switch: false,
        });
        let body = self.peel(body);
        self.compile_stmt(&body);
        // `continue` lands here: update, then re-test.
        let update_start = self.code.len();
        let update = self.peel(update);
        self.compile_expr(&update);
        self.emit_simple(Op::DropTop, node);
        self.emit(Op::JumpBack, Operand::Addr(top), node);
        self.patch(end_jump);
        let end = self.code.len();
        let ctx = self.cur.as_mut().expect("no function").loops.pop().unwrap();
        for p in ctx.break_patches {
            self.patch_to(p, end);
        }
        for p in ctx.continue_patches {
            self.patch_to(p, update_start);
        }
    }

    fn compile_foreach(&mut self, node: &Node<'a>) {
        let needle = node.child_by_field_name("needle");
        let hay = node.child_by_field_name("hay_stack");
        let (Some(needle), Some(hay_field)) = (needle, hay) else {
            self.fail(node, "bad foreach statement".to_string());
            return;
        };
        // `foreach` parses (the grammar covers newer games too) but the
        // Call of Duty 1 / United Offensive script VM has no such
        // statement. Report it; codegen continues so nothing else is lost.
        self.fail(
            node,
            "foreach is not supported in Call of Duty 1 / United Offensive GSC, use a for loop".to_string(),
        );
        let hay_id = hay_field.id();
        let needle_id = needle.id();
        let hay = self.peel(hay_field);
        if let Some(v) = self.static_value(&hay) {
            if !matches!(v, Value::Array(_)) {
                self.fail(&hay, format!("foreach needs an array, got {}", v.type_name()));
            }
        }
        self.compile_expr(&hay);
        let arr = self.temp_slot();
        self.emit(Op::SetLocal, Operand::Slot(arr), node);
        let idx = self.temp_slot();
        self.emit(Op::GetInt, Operand::Int(0), node);
        self.emit(Op::SetLocal, Operand::Slot(idx), node);
        let top = self.code.len();
        // while idx < size(arr)
        self.emit(Op::GetLocal, Operand::Slot(idx), node);
        self.emit(Op::GetLocal, Operand::Slot(arr), node);
        self.emit_simple(Op::Size, node);
        self.emit_simple(Op::Less, node);
        let end_jump = self.emit_jump(Op::JumpOnFalse, node);
        self.cur.as_mut().expect("no function").loops.push(LoopCtx {
            break_patches: Vec::new(),
            continue_target: Some(top),
            continue_patches: Vec::new(),
            is_switch: false,
        });
        // needle = arr[idx]
        self.emit(Op::GetLocal, Operand::Slot(arr), node);
        self.emit(Op::GetLocal, Operand::Slot(idx), node);
        self.emit_simple(Op::GetIndex, node);
        let needle_name = self.text(&needle).to_string();
        let slot = self.local_slot(&needle_name);
        self.emit(Op::SetLocal, Operand::Slot(slot), node);
        // body: the named child that is neither needle nor hay_stack.
        for child in self.named_children(node) {
            if child.id() == needle_id || child.id() == hay_id {
                continue;
            }
            let body = self.peel(child);
            self.compile_stmt(&body);
        }
        // idx++
        self.emit(Op::GetLocal, Operand::Slot(idx), node);
        self.emit_simple(Op::Inc, node);
        self.emit(Op::SetLocal, Operand::Slot(idx), node);
        self.emit(Op::JumpBack, Operand::Addr(top), node);
        self.patch(end_jump);
        self.close_loop(self.code.len());
    }

    fn compile_switch(&mut self, node: &Node<'a>) {
        let tested = node.child_by_field_name("tested");
        let Some(tested) = tested else {
            self.fail(node, "bad switch statement".to_string());
            return;
        };
        let tested = self.peel(tested);
        self.compile_expr(&tested);
        let subj = self.temp_slot();
        self.emit(Op::SetLocal, Operand::Slot(subj), node);
        self.cur.as_mut().expect("no function").loops.push(LoopCtx {
            break_patches: Vec::new(),
            continue_target: None,
            continue_patches: Vec::new(),
            is_switch: true,
        });
        // The grammar repeats the `switch_block` field, so cases and
        // defaults arrive as separate field children (braces included).
        let mut cursor = node.walk();
        let clauses: Vec<Node<'a>> = node
            .children_by_field_name("switch_block", &mut cursor)
            .filter(|c| c.is_named())
            .collect();
        let mut next_jumps: Vec<usize> = Vec::new();
        for child in clauses {
            match child.kind() {
                "case_statement" => {
                    let Some(val_field) = child.child_by_field_name("possible_value") else {
                        self.fail(&child, "bad case".to_string());
                        continue;
                    };
                    let val_id = val_field.id();
                    for p in next_jumps.drain(..) {
                        self.patch(p);
                    }
                    self.emit(Op::GetLocal, Operand::Slot(subj), &child);
                    let val = self.peel(val_field);
                    self.compile_expr(&val);
                    self.emit_simple(Op::Equal, &child);
                    next_jumps.push(self.emit_jump(Op::JumpOnFalse, &child));
                    for stmt in self.named_children(&child) {
                        if stmt.id() == val_id {
                            continue;
                        }
                        let s = self.peel(stmt);
                        self.compile_stmt(&s);
                    }
                }
                "default_statement" => {
                    for p in next_jumps.drain(..) {
                        self.patch(p);
                    }
                    for stmt in self.named_children(&child) {
                        let s = self.peel(stmt);
                        self.compile_stmt(&s);
                    }
                }
                _ => {}
            }
        }
        for p in next_jumps.drain(..) {
            self.patch(p);
        }
        let end = self.code.len();
        let ctx = self.cur.as_mut().expect("no function").loops.pop().unwrap();
        for p in ctx.break_patches {
            self.patch_to(p, end);
        }
    }
}

#[derive(Debug, Clone)]
enum VarRef {
    Local(u32),
    Global(u32),
    NewLocal(String),
}

/// Expression-like node kinds (after [`Compiler::peel`]).
fn is_value_kind(kind: &str) -> bool {
    matches!(
        kind,
        "binary_expression"
            | "unary_expression"
            | "direct_call"
            | "thread_call"
            | "object_call"
            | "pointer_call"
            | "function_pointer"
            | "local_function_ptr"
            | "foreign_function_ptr"
            | "stored_func_ref"
            | "member_expression"
            | "cast_expression"
            | "array_access"
            | "assignment_expression"
            | "postfix_expression"
            | "identifier"
            | "number"
            | "string"
            | "lstring"
            | "boolean"
            | "undefined"
            | "vec1"
            | "vec3"
            | "array"
    )
}

/// Unquote a `"..."` literal, processing `\\` escapes.
fn unquote(text: &str) -> String {
    let inner = text.strip_prefix('\"').unwrap_or(text);
    let inner = inner.strip_suffix('\"').unwrap_or(inner);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Byte offset to LSP range.
pub(crate) fn byte_range_to_range(
    src: &str,
    start: usize,
    end: usize,
) -> tower_lsp_server::lsp_types::Range {
    use tower_lsp_server::lsp_types::Position;
    let mut line = 0u32;
    let mut character = 0u32;
    let mut start_pos = Position::new(0, 0);
    let mut end_pos = Position::new(0, 0);
    for (i, ch) in src.char_indices() {
        if i == start {
            start_pos = Position::new(line, character);
        }
        if i == end {
            end_pos = Position::new(line, character);
            break;
        }
        if ch == '\n' {
            line += 1;
            character = 0;
        } else {
            character += 1;
        }
    }
    if end >= src.len() {
        end_pos = Position::new(line, character);
    }
    tower_lsp_server::lsp_types::Range::new(start_pos, end_pos)
}

impl<'a> Compiler<'a> {
    // -- expressions: each leaves exactly one value on the stack ---

    fn compile_expr(&mut self, node: &Node<'a>) {
        let node = self.peel(*node);
        match node.kind() {
            "number" => self.compile_number(&node),
            "string" => {
                let s = unquote(self.text(&node));
                self.emit(Op::GetString, Operand::Str(s), &node);
            }
            "lstring" => {
                // `&"..."`: the inner string child holds the text.
                let mut inner = self.text(&node).to_string();
                for child in self.named_children(&node) {
                    if child.kind() == "string" {
                        inner = unquote(self.text(&child));
                    }
                }
                self.emit(Op::GetIStr, Operand::Str(inner), &node);
            }
            "boolean" => {
                let v = if self.text(&node) == "true" { 1 } else { 0 };
                self.emit(Op::GetInt, Operand::Int(v), &node);
            }
            "undefined" => self.emit_simple(Op::GetUndefined, &node),
            "array" => self.emit_simple(Op::NewArray, &node),
            "vec1" => {
                for child in self.named_children(&node) {
                    let e = self.peel(child);
                    self.compile_expr(&e);
                }
            }
            "vec3" => {
                let mut n = 0;
                for child in self.named_children(&node) {
                    let e = self.peel(child);
                    if let Some(v) = self.static_value(&e) {
                        if v.cast_float().is_err() {
                            self.fail(&e, format!("vector needs numbers, got {}", v.type_name()));
                        }
                    }
                    self.compile_expr(&e);
                    n += 1;
                }
                if n != 3 {
                    self.fail(&node, "vector needs 3 components".to_string());
                    return;
                }
                self.emit_simple(Op::Vector, &node);
            }
            "identifier" => self.compile_identifier(&node),
            "binary_expression" => self.compile_binary(&node),
            "unary_expression" => self.compile_unary(&node),
            "postfix_expression" => self.compile_postfix(&node),
            "assignment_expression" => self.compile_assignment(&node),
            "direct_call" | "thread_call" => self.compile_direct_call(&node),
            "object_call" => self.compile_object_call(&node),
            "pointer_call" => self.compile_pointer_call(&node),
            "local_function_ptr" => {
                if let Some(f) = node.child_by_field_name("function") {
                    let name = self.text(&f).to_lowercase();
                    self.emit(Op::GetFuncRef, Operand::Name(name), &node);
                }
            }
            "function_pointer" => {
                // Wrapper around a local or foreign reference.
                for child in self.named_children(&node) {
                    let e = self.peel(child);
                    self.compile_expr(&e);
                }
            }
            "foreign_function_ptr" => {
                self.emit(
                    Op::NeedGame,
                    Operand::Name("foreign calls need a linked script".to_string()),
                    &node,
                );
                self.emit_simple(Op::GetUndefined, &node);
            }
            "stored_func_ref" => {
                for child in self.named_children(&node) {
                    let e = self.peel(child);
                    if e.kind() == "identifier" {
                        self.compile_identifier(&e);
                    } else {
                        self.compile_expr(&e);
                    }
                }
            }
            "member_expression" => self.compile_member_read(&node),
            "array_access" => self.compile_array_read(&node),
            "cast_expression" => {
                let ty = node.child_by_field_name("type_name");
                let var = node.child_by_field_name("var");
                let (Some(ty), Some(var)) = (ty, var) else {
                    self.fail(&node, "bad cast".to_string());
                    return;
                };
                let var = self.peel(var);
                // `var` is one of number/string/boolean/identifier/...:
                // compile it as a general expression.
                if let Some(v) = self.static_value(&var) {
                    let bad = match self.text(&ty) {
                        "int" => v.cast_int().err(),
                        "float" => v.cast_float().err(),
                        "bool" => v.cast_bool().err(),
                        "string" => v.cast_string().err(),
                        _ => None,
                    };
                    if let Some(message) = bad {
                        self.fail(&var, message);
                    }
                }
                self.compile_expr(&var);
                match self.text(&ty) {
                    "int" => self.emit_simple(Op::CastInt, &node),
                    "float" => self.emit_simple(Op::CastFloat, &node),
                    "bool" => self.emit_simple(Op::CastBool, &node),
                    "string" => self.emit_simple(Op::CastString, &node),
                    t => self.fail(&node, format!("bad cast type {t}")),
                };
            }
            _ => self.fail(&node, format!("cannot compile {}", node.kind())),
        }
    }

    /// Statically known value of an expression: literals yield their
    /// actual values, builtin calls their declared return shape, and
    /// pure compositions fold. Anything touching variables, the game,
    /// or user functions is `None`: those rely on runtime.
    fn static_value(&self, node: &Node<'a>) -> Option<Value> {
        let node = self.peel(*node);
        match node.kind() {
            "number" => self.static_number(&node),
            "string" => Some(Value::Str(unquote(self.text(&node)))),
            "lstring" => {
                let mut inner = String::new();
                for child in self.named_children(&node) {
                    if child.kind() == "string" {
                        inner = unquote(self.text(&child));
                    }
                }
                Some(Value::IStr(inner))
            }
            "boolean" => Some(Value::Int(i32::from(self.text(&node) == "true"))),
            "undefined" => Some(Value::Undefined),
            "array" => Some(Value::Array(std::rc::Rc::new(std::cell::RefCell::new(Vec::new())))),
            "vec1" => {
                let mut out = None;
                for child in self.named_children(&node) {
                    out = self.static_value(&child);
                }
                out
            }
            "vec3" => {
                let mut comps: Vec<f32> = Vec::new();
                for child in self.named_children(&node) {
                    let v = self.static_value(&child)?;
                    comps.push(v.cast_float().ok()?);
                }
                if comps.len() == 3 {
                    Some(Value::Vec3([comps[0], comps[1], comps[2]]))
                } else {
                    None
                }
            }
            "direct_call" => {
                let mut callee = None;
                for child in self.named_children(&node) {
                    if child.kind() == "identifier" {
                        callee = Some(child);
                    }
                }
                let name = self.text(&callee?).to_lowercase();
                let sigs = builtin_sigs();
                let f = sigs.functions.get(&name)?;
                f.returns.as_ref().and_then(return_rep)
            }
            "object_call" => {
                let method = self.object_method(&node)?;
                let sigs = builtin_sigs();
                let m = sigs.methods.get(&method)?;
                m.returns.as_ref().and_then(return_rep)
            }
            "binary_expression" => {
                let (l, r) = self.binary_operands(&node)?;
                let op = self.src[l.end_byte()..r.start_byte()].trim();
                let (Some(lv), Some(rv)) = (self.static_value(&l), self.static_value(&r)) else {
                    return None;
                };
                match op {
                    "&&" | "||" => Some(Value::Int(0)),
                    "==" | "!=" => Value::equals(&lv, &rv).ok().map(|b| Value::Int(i32::from(b))),
                    "<" | ">" | "<=" | ">=" => Value::compare(op, &lv, &rv)
                        .ok()
                        .map(|b| Value::Int(i32::from(b))),
                    "+" | "-" | "*" | "/" | "%" => Value::arith(op.chars().next()?, &lv, &rv).ok(),
                    "<<" | ">>" | "|" | "^" | "&" => {
                        if matches!((&lv, &rv), (Value::Int(_), Value::Int(_))) {
                            Some(Value::Int(0))
                        } else {
                            None
                        }
                    }
                    _ => None,
                }
            }
            "unary_expression" => {
                let inner = self.unary_inner(&node)?;
                let op = self.src[node.start_byte()..inner.start_byte()].trim();
                let v = self.static_value(&inner)?;
                match op {
                    "+" => Some(v),
                    "-" => match v {
                        Value::Int(i) => Some(Value::Int(i.wrapping_neg())),
                        Value::Float(f) => Some(Value::Float(-f)),
                        _ => None,
                    },
                    "!" => Some(Value::Int(0)),
                    "~" => matches!(v, Value::Int(_)).then_some(Value::Int(0)),
                    _ => None,
                }
            }
            "cast_expression" => {
                let ty = node.child_by_field_name("type_name")?;
                let var = node.child_by_field_name("var")?;
                let v = self.static_value(&var)?;
                match self.text(&ty) {
                    "int" => v.cast_int().ok().map(Value::Int),
                    "float" => v.cast_float().ok().map(Value::Float),
                    "bool" => v.cast_bool().ok().map(Value::Int),
                    "string" => v.cast_string().ok().map(Value::Str),
                    _ => None,
                }
            }
            "assignment_expression" => {
                let val = node.child_by_field_name("assigned_value")?;
                self.static_value(&val)
            }
            _ => None,
        }
    }

    fn static_number(&self, node: &Node) -> Option<Value> {
        let t = self.text(node);
        if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
            return i32::from_str_radix(hex, 16).ok().map(Value::Int);
        }
        if t.contains('.') {
            return t.parse::<f32>().ok().map(Value::Float);
        }
        t.parse::<i32>().ok().map(Value::Int)
    }

    /// The two operand nodes of a binary expression.
    fn binary_operands(&self, node: &Node<'a>) -> Option<(Node<'a>, Node<'a>)> {
        let mut kids = self.named_children(node);
        if kids.len() != 2 {
            return None;
        }
        let rhs = self.peel(kids.pop().unwrap());
        let lhs = self.peel(kids.pop().unwrap());
        Some((lhs, rhs))
    }

    /// The single operand node of a unary expression.
    fn unary_inner(&self, node: &Node<'a>) -> Option<Node<'a>> {
        let mut kids = self.named_children(node);
        if kids.len() != 1 {
            return None;
        }
        Some(self.peel(kids.pop().unwrap()))
    }

    /// The method name of an object call, if syntactically present.
    fn object_method(&self, node: &Node<'a>) -> Option<String> {
        let mut seen_obj = false;
        for child in self.named_children(node) {
            match child.kind() {
                "identifier" => {
                    if seen_obj {
                        return Some(self.text(&child).to_lowercase());
                    }
                    seen_obj = true;
                }
                "local_function_ptr" => {
                    if let Some(f) = child.child_by_field_name("function") {
                        return Some(self.text(&f).to_lowercase());
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn compile_number(&mut self, node: &Node<'a>) {
        let t = self.text(node);
        if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
            match i32::from_str_radix(hex, 16) {
                Ok(v) => self.emit(Op::GetInt, Operand::Int(v), node),
                Err(_) => self.fail(node, format!("bad number {t}")),
            };
        } else if t.contains('.') {
            match t.parse::<f32>() {
                Ok(v) => self.emit(Op::GetFloat, Operand::Float(v), node),
                Err(_) => self.fail(node, format!("bad number {t}")),
            };
        } else {
            match t.parse::<i32>() {
                Ok(v) => self.emit(Op::GetInt, Operand::Int(v), node),
                Err(_) => self.fail(node, format!("bad number {t}")),
            };
        }
    }

    fn compile_identifier(&mut self, node: &Node<'a>) {
        let name = self.text(node).to_string();
        match name.as_str() {
            "self" | "level" | "game" | "anim" => {
                self.emit(
                    Op::NeedGame,
                    Operand::Name(format!("{name} needs a game attached")),
                    node,
                );
                self.emit_simple(Op::GetUndefined, node);
            }
            _ => {
                let r = self.resolve(&name);
                self.emit_get_var(&r, node);
            }
        }
    }

    fn compile_binary(&mut self, node: &Node<'a>) {
        let mut kids = self.named_children(node);
        if kids.len() != 2 {
            self.fail(node, "bad binary expression".to_string());
            return;
        }
        let rhs = self.peel(kids.pop().unwrap());
        let lhs = self.peel(kids.pop().unwrap());
        // Operator text sits between the operands.
        let op = self.src[lhs.end_byte()..rhs.start_byte()].trim().to_string();
        self.check_binary(&lhs, &rhs, &op, node);
        match op.as_str() {
            "&&" => {
                self.compile_expr(&lhs);
                let f = self.emit_jump(Op::JumpOnFalse, node);
                self.compile_expr(&rhs);
                self.emit_simple(Op::CastBool, node);
                let e = self.emit_jump(Op::Jump, node);
                self.patch(f);
                self.emit(Op::GetInt, Operand::Int(0), node);
                self.patch(e);
            }
            "||" => {
                self.compile_expr(&lhs);
                let t = self.emit_jump(Op::JumpOnTrue, node);
                self.compile_expr(&rhs);
                self.emit_simple(Op::CastBool, node);
                let e = self.emit_jump(Op::Jump, node);
                self.patch(t);
                self.emit(Op::GetInt, Operand::Int(1), node);
                self.patch(e);
            }
            _ => {
                self.compile_expr(&lhs);
                self.compile_expr(&rhs);
                let op = match op.as_str() {
                    "==" => Op::Equal,
                    "!=" => Op::NotEqual,
                    "<" => Op::Less,
                    ">" => Op::Greater,
                    "<=" => Op::LessEqual,
                    ">=" => Op::GreaterEqual,
                    "<<" => Op::ShiftLeft,
                    ">>" => Op::ShiftRight,
                    "+" => Op::Plus,
                    "-" => Op::Minus,
                    "*" => Op::Multiply,
                    "/" => Op::Divide,
                    "%" => Op::Modulo,
                    "|" => Op::BitOr,
                    "^" => Op::BitXor,
                    "&" => Op::BitAnd,
                    _ => {
                        self.fail(node, format!("bad operator {op}"));
                        return;
                    }
                };
                self.emit_simple(op, node);
            }
        }
    }

    /// Static type check for a binary operation. Both sides must be
    /// statically known; anything relying on runtime is skipped.
    /// Reports the engine's own error text on mismatch.
    fn check_binary(&mut self, lhs: &Node<'a>, rhs: &Node<'a>, op: &str, node: &Node) {
        let (Some(lv), Some(rv)) = (self.static_value(lhs), self.static_value(rhs)) else {
            return;
        };
        let err = match op {
            "&&" => {
                // The right side only runs when the left is true.
                lv.truthy()
                    .err()
                    .or_else(|| {
                        if lv.truthy().unwrap_or(false) {
                            rv.truthy().err()
                        } else {
                            None
                        }
                    })
            }
            "||" => {
                lv.truthy()
                    .err()
                    .or_else(|| {
                        if !lv.truthy().unwrap_or(true) {
                            rv.truthy().err()
                        } else {
                            None
                        }
                    })
            }
            "==" | "!=" => Value::equals(&lv, &rv).err(),
            "<" | ">" | "<=" | ">=" => Value::compare(op, &lv, &rv).err(),
            "+" | "-" | "*" | "/" | "%" => {
                Value::arith(op.chars().next().unwrap(), &lv, &rv).err()
            }
            "<<" | ">>" | "|" | "^" | "&" => Self::check_int_only(&lv, &rv)
                .map(|kind| format!("{kind} needs integer operands")),
            _ => None,
        };
        if let Some(message) = err {
            self.fail(node, message);
        }
    }

    /// Shifts and bitwise ops need integers; floats may coerce at
    /// runtime, so only certainly-wrong kinds are flagged.
    fn check_int_only(lv: &Value, rv: &Value) -> Option<&'static str> {
        fn bad(v: &Value) -> bool {
            matches!(
                v,
                Value::Str(_) | Value::Vec3(_) | Value::Array(_) | Value::Entity | Value::Undefined
            )
        }
        if bad(lv) || bad(rv) {
            Some("operator")
        } else {
            None
        }
    }

    /// Report if a known value cannot serve as a condition.
    fn check_truthy(&mut self, v: &Value, node: &Node) {
        if let Err(message) = v.truthy() {
            self.fail(node, message);
        }
    }

    /// Array indices must be integers; floats may truncate at runtime
    /// and are left alone, everything else is certainly wrong.
    fn check_index(&mut self, idx: &Node<'a>) {
        if let Some(v) = self.static_value(idx) {
            if !matches!(v, Value::Int(_) | Value::Float(_)) {
                self.fail(idx, format!("array index must be an integer, got {}", v.type_name()));
            }
        }
    }

    fn compile_unary(&mut self, node: &Node<'a>) {
        let mut kids = self.named_children(node);
        if kids.len() != 1 {
            self.fail(node, "bad unary expression".to_string());
            return;
        }
        let inner = self.peel(kids.pop().unwrap());
        let op = self.src[node.start_byte()..inner.start_byte()].trim().to_string();
        if let Some(v) = self.static_value(&inner) {
            match op.as_str() {
                "!" => self.check_truthy(&v, &inner),
                "~" if !matches!(v, Value::Int(_)) => {
                    self.fail(&inner, format!("~ cannot be applied to \"{}\"", v.type_name()));
                }
                "-" => {
                    if Value::arith('-', &Value::Int(0), &v).is_err() {
                        self.fail(&inner, format!("cannot negate {}", v.type_name()));
                    }
                }
                _ => {}
            }
        }
        match op.as_str() {
            "!" => {
                self.compile_expr(&inner);
                self.emit_simple(Op::BoolNot, node);
            }
            "~" => {
                self.compile_expr(&inner);
                self.emit_simple(Op::BitNot, node);
            }
            "+" => self.compile_expr(&inner),
            "-" => {
                // No negate opcode exists; the engine folds this at
                // compile time, `0 - x` is the faithful lowering.
                self.emit(Op::GetInt, Operand::Int(0), node);
                self.compile_expr(&inner);
                self.emit_simple(Op::Minus, node);
            }
            _ => self.fail(node, format!("bad operator {op}")),
        }
    }

    fn compile_postfix(&mut self, node: &Node<'a>) {
        let mut kids = self.named_children(node);
        if kids.len() != 1 {
            self.fail(node, "bad postfix expression".to_string());
            return;
        }
        let target = self.peel(kids.pop().unwrap());
        let inc = self.src[target.end_byte()..node.end_byte()].contains("++");
        let op = if inc { Op::Inc } else { Op::Dec };
        match target.kind() {
            "identifier" => {
                let name = self.text(&target).to_string();
                let r = self.resolve(&name);
                self.emit_get_var(&r, &target);
                self.emit_simple(Op::Dup, node);
                self.emit_simple(op, node);
                self.emit_store_resolved(&name, &r, node);
            }
            _ => {
                // Complex target: evaluate the address once via temps.
                let Some((base, idx)) = self.compile_addr(&target) else {
                    return;
                };
                self.push_addr(&base, &idx, node);
                self.emit_simple(Op::GetIndex, node);
                let t_o = self.temp_slot();
                self.emit(Op::SetLocal, Operand::Slot(t_o), node);
                self.emit(Op::GetLocal, Operand::Slot(t_o), node);
                self.emit_simple(op, node);
                let t_n = self.temp_slot();
                self.emit(Op::SetLocal, Operand::Slot(t_n), node);
                self.push_addr(&base, &idx, node);
                self.emit(Op::GetLocal, Operand::Slot(t_n), node);
                self.emit_simple(Op::SetIndex, node);
                // Postfix yields the old value.
                self.emit(Op::GetLocal, Operand::Slot(t_o), node);
            }
        }
    }

    fn compile_assignment(&mut self, node: &Node<'a>) {
        let var = node.child_by_field_name("variable");
        let val = node.child_by_field_name("assigned_value");
        let (Some(var), Some(val)) = (var, val) else {
            self.fail(node, "bad assignment".to_string());
            return;
        };
        let var = self.peel(var);
        let val = self.peel(val);
        let op = self.src[var.end_byte()..val.start_byte()].trim().to_string();
        if op == "=" {
            self.compile_assign_value(&var, &val, node);
            return;
        }
        // Compound assignment: read, operate, store.
        let arith = match op.as_str() {
            "+=" => Op::Plus,
            "-=" => Op::Minus,
            "*=" => Op::Multiply,
            "/=" => Op::Divide,
            "%=" => Op::Modulo,
            "&=" => Op::BitAnd,
            "|=" => Op::BitOr,
            "^=" => Op::BitXor,
            "<<=" => Op::ShiftLeft,
            ">>=" => Op::ShiftRight,
            _ => {
                self.fail(node, format!("bad operator {op}"));
                return;
            }
        };
        match var.kind() {
            "identifier" => {
                let name = self.text(&var).to_string();
                let r = self.resolve(&name);
                self.emit_get_var(&r, &var);
                self.compile_expr(&val);
                self.emit_simple(arith, node);
                self.emit_simple(Op::Dup, node);
                self.emit_store_resolved(&name, &r, node);
            }
            "member_expression" => {
                let Some((base, field)) = self.split_member(&var) else {
                    return;
                };
                self.compile_expr(&base);
                self.emit_simple(Op::Dup, node);
                self.emit(Op::GetField, Operand::Name(field.clone()), node);
                self.compile_expr(&val);
                self.emit_simple(arith, node);
                let t = self.temp_slot();
                self.emit(Op::SetLocal, Operand::Slot(t), node);
                // Stack is [base]; push the new value for the store.
                self.emit(Op::GetLocal, Operand::Slot(t), node);
                self.emit(Op::SetField, Operand::Name(field), node);
                self.emit(Op::GetLocal, Operand::Slot(t), node);
            }
            _ => {
                let Some((base, idx)) = self.compile_addr(&var) else {
                    return;
                };
                self.emit_simple(Op::GetIndex, node);
                self.compile_expr(&val);
                self.emit_simple(arith, node);
                let t = self.temp_slot();
                self.emit(Op::SetLocal, Operand::Slot(t), node);
                self.push_addr(&base, &idx, node);
                self.emit(Op::GetLocal, Operand::Slot(t), node);
                self.emit_simple(Op::SetIndex, node);
                // Compound assignment yields the new value.
                self.emit(Op::GetLocal, Operand::Slot(t), node);
            }
        }
    }

    /// Plain `target = value` store. The value stays on the stack.
    fn compile_assign_value(&mut self, var: &Node<'a>, val: &Node<'a>, node: &Node<'a>) {
        match var.kind() {
            "identifier" => {
                let name = self.text(var).to_string();
                if is_game_object(&name) {
                    self.fail(var, format!("{name} needs a game attached"));
                    self.emit_simple(Op::GetUndefined, node);
                    return;
                }
                self.compile_expr(val);
                self.emit_set_var(&name, node);
            }
            "member_expression" => {
                let Some((base, field)) = self.split_member(var) else {
                    return;
                };
                if base.kind() == "identifier" && is_game_object(self.text(&base)) {
                    self.emit(
                        Op::NeedGame,
                        Operand::Name(format!("{} needs a game attached", self.text(&base))),
                        node,
                    );
                    self.emit_simple(Op::GetUndefined, node);
                    return;
                }
                // Evaluate once into temps: the store consumes its inputs.
                self.compile_expr(&base);
                let t_b = self.temp_slot();
                self.emit(Op::SetLocal, Operand::Slot(t_b), node);
                self.compile_expr(val);
                let t_v = self.temp_slot();
                self.emit(Op::SetLocal, Operand::Slot(t_v), node);
                self.emit(Op::GetLocal, Operand::Slot(t_b), node);
                self.emit(Op::GetLocal, Operand::Slot(t_v), node);
                self.emit(Op::SetField, Operand::Name(field), node);
                self.emit(Op::GetLocal, Operand::Slot(t_v), node);
            }
            _ => {
                let Some((base, idx)) = self.compile_addr(var) else {
                    // compile_addr already reported.
                    self.emit_simple(Op::GetUndefined, node);
                    return;
                };
                self.push_addr(&base, &idx, node);
                self.compile_expr(val);
                // The store consumes address and value; park the value
                // in a temp so the assignment still yields it.
                let t_v = self.temp_slot();
                self.emit_simple(Op::SetIndex, node);
                self.emit(Op::GetLocal, Operand::Slot(t_v), node);
            }
        }
    }

    /// Store the stack top into an already-resolved variable (no Dup).
    fn emit_store_resolved(&mut self, name: &str, r: &VarRef, node: &Node<'a>) {
        match r.clone() {
            VarRef::Local(i) => {
                self.emit(Op::SetLocal, Operand::Slot(i), node);
            }
            VarRef::Global(i) => {
                self.emit(Op::SetGlobal, Operand::Slot(i), node);
            }
            VarRef::NewLocal(_) => {
                let i = self.local_slot(name);
                self.emit(Op::SetLocal, Operand::Slot(i), node);
            }
        }
    }

    /// Compile the address of an array element: base value plus index
    /// values stay available for re-emission via the returned parts.
    fn compile_addr(&mut self, node: &Node<'a>) -> Option<(Node<'a>, Vec<Node<'a>>)> {
        if node.kind() != "array_access" {
            self.fail(node, "cannot assign to this".to_string());
            return None;
        }
        let mut kids = self.named_children(node);
        if kids.is_empty() {
            self.fail(node, "bad array access".to_string());
            return None;
        }
        let base = self.peel(kids.remove(0));
        let mut indices = Vec::new();
        for k in kids {
            indices.push(self.peel(k));
        }
        if indices.is_empty() {
            self.fail(node, "bad array access".to_string());
            return None;
        }
        Some((base, indices))
    }

    /// Re-emit a previously compiled address (base + all but last index
    /// resolved, last index pending) for the final store.
    fn push_addr(&mut self, base: &Node<'a>, indices: &[Node<'a>], node: &Node<'a>) {
        self.compile_expr(base);
        for idx in &indices[..indices.len().saturating_sub(1)] {
            self.check_index(idx);
            self.compile_expr(idx);
            self.emit_simple(Op::GetIndex, node);
        }
        if let Some(last) = indices.last() {
            self.check_index(last);
            self.compile_expr(last);
        }
    }

    /// Split `a.b.c` into (`a.b`, `c`).
    fn split_member(&mut self, node: &Node<'a>) -> Option<(Node<'a>, String)> {
        let member = node.child_by_field_name("member")?;
        // The object is everything except the last `.member`.
        let mut obj = None;
        if let Some(o) = node.child_by_field_name("object") {
            obj = Some(self.peel(o));
        } else if let Some(p) = node.child_by_field_name("parent_member_expr") {
            obj = Some(self.peel(p));
        }
        Some((obj?, self.text(&member).to_string()))
    }
}

/// `self`, `level`, `game` and `anim` need a game world attached.
fn is_game_object(name: &str) -> bool {
    matches!(name, "self" | "level" | "game" | "anim")
}

impl<'a> Compiler<'a> {
    fn compile_call_args(&mut self, node: &Node<'a>) -> usize {
        for child in self.named_children(node) {
            if child.kind() == "argument_list" {
                let mut n = 0;
                for arg in self.named_children(&child) {
                    let e = self.peel(arg);
                    self.compile_expr(&e);
                    n += 1;
                }
                return n;
            }
        }
        0
    }

    fn compile_direct_call(&mut self, node: &Node<'a>) {
        let threaded = node.kind() == "thread_call";
        // Callee: identifier or foreign_function_ptr.
        let mut callee = None;
        let mut foreign = false;
        for child in self.named_children(node) {
            match child.kind() {
                "identifier" => callee = Some(child),
                "foreign_function_ptr" => foreign = true,
                _ => {}
            }
        }
        if foreign || callee.is_none() {
            self.emit(
                Op::NeedGame,
                Operand::Name("foreign calls need a linked script".to_string()),
                node,
            );
            self.emit_simple(Op::GetUndefined, node);
            return;
        }
        let callee = callee.unwrap();
        let name = self.text(&callee).to_lowercase();
        if threaded {
            self.emit(
                Op::NeedGame,
                Operand::Name(format!("thread {name} needs a game scheduler")),
                node,
            );
            self.emit_simple(Op::GetUndefined, node);
            return;
        }
        let argc = self.compile_call_args(node);
        if self.functions.contains_key(&name) {
            self.emit(Op::CallFunction, Operand::FuncCall { name, argc }, node);
        } else {
            // Builtin (pure or engine), or a function from another file:
            // resolved at runtime so the file still compiles standalone.
            // Arity is checked only when the engine provably enforces it.
            if let Some((min, max)) = builtin::arity(&name) {
                let bad = argc < min || max.is_some_and(|m| argc > m);
                if bad {
                    let expected = match max {
                        Some(m) if m == min => format!("{min} parameters"),
                        Some(m) => format!("{min} to {m} parameters"),
                        None => format!("at least {min} parameters"),
                    };
                    self.fail(
                        &callee,
                        format!("{name} expects {expected}, got {argc}"),
                    );
                }
            }
            // Declared parameter types are checked against statically
            // known arguments; anything relying on runtime is skipped.
            if let Some(f) = builtin_sigs().functions.get(&name) {
                self.check_call_args(&name, &f.params, node);
            }
            self.emit(Op::BuiltinFunction, Operand::FuncCall { name, argc }, node);
        }
    }

    /// Check statically known call arguments against declared builtin
    /// parameter types. Extra arguments (variadics) and missing ones
    /// (engine-tolerated optionals) are never flagged.
    fn check_call_args(&mut self, fname: &str, params: &[crate::doc::ScrParam], node: &Node<'a>) {
        let mut args = Vec::new();
        for child in self.named_children(node) {
            if child.kind() == "argument_list" {
                for arg in self.named_children(&child) {
                    args.push(self.peel(arg));
                }
            }
        }
        for (param, arg) in params.iter().zip(args.iter()) {
            if let Some(v) = self.static_value(arg) {
                if let Some(message) = arg_mismatch(&param.ptype, &v, fname, &param.name) {
                    self.fail(arg, message);
                }
            }
        }
    }

    fn compile_object_call(&mut self, node: &Node<'a>) {
        let mut obj = None;
        let mut method = None;
        for child in self.named_children(node) {
            match child.kind() {
                "identifier" => {
                    if obj.is_none() {
                        obj = Some(child);
                    } else if method.is_none() {
                        method = Some(child);
                    }
                }
                "local_function_ptr" | "foreign_function_ptr" => method = Some(child),
                _ => {}
            }
        }
        let (Some(obj), Some(method)) = (obj, method) else {
            self.fail(node, "bad method call".to_string());
            return;
        };
        let obj_name = self.text(&obj).to_string();
        if is_game_object(&obj_name) {
            self.emit(
                Op::NeedGame,
                Operand::Name(format!("{obj_name} needs a game attached")),
                node,
            );
            self.emit_simple(Op::GetUndefined, node);
            return;
        }
        let method_name = if method.kind() == "identifier" {
            self.text(&method).to_lowercase()
        } else if method.kind() == "local_function_ptr" {
            if let Some(f) = method.child_by_field_name("function") {
                self.text(&f).to_lowercase()
            } else {
                self.fail(node, "bad method call".to_string());
                return;
            }
        } else {
            self.emit(
                Op::NeedGame,
                Operand::Name("foreign calls need a linked script".to_string()),
                node,
            );
            self.emit_simple(Op::GetUndefined, node);
            return;
        };
        if is_builtin_method(&method_name) {
            self.emit(
                Op::NeedGame,
                Operand::Name(format!("builtin method {method_name} needs a game attached")),
                node,
            );
            self.emit_simple(Op::GetUndefined, node);
            return;
        }
        // Local `obj method(args)`: the object becomes `self`.
        let obj = self.peel(obj);
        self.compile_expr(&obj);
        let argc = self.compile_call_args(node);
        self.emit(Op::CallMethod, Operand::FuncCall { name: method_name, argc }, node);
    }

    fn compile_pointer_call(&mut self, node: &Node<'a>) {
        let mut target = None;
        for child in self.named_children(node) {
            match child.kind() {
                "stored_func_ref" => {
                    target = Some(child);
                    break;
                }
                _ => {}
            }
        }
        let Some(target) = target else {
            self.fail(node, "bad pointer call".to_string());
            return;
        };
        // [[expr]]: stash the reference in a temp first, so it sits
        // below the arguments for CallPointer to pop last.
        let t = self.temp_slot();
        let mut done = false;
        for child in self.named_children(&target) {
            let e = self.peel(child);
            if let Some(v) = self.static_value(&e) {
                if !matches!(v, Value::Func(_)) {
                    self.fail(&e, format!("[[...]] needs a function, got {}", v.type_name()));
                }
            }
            if e.kind() == "identifier" {
                self.compile_identifier(&e);
            } else if e.kind() == "foreign_function_ptr" {
                self.emit(
                    Op::NeedGame,
                    Operand::Name("foreign calls need a linked script".to_string()),
                    node,
                );
                self.emit_simple(Op::GetUndefined, node);
            } else {
                self.compile_expr(&e);
            }
            done = true;
        }
        if !done {
            self.fail(node, "bad pointer call".to_string());
            return;
        }
        self.emit(Op::SetLocal, Operand::Slot(t), node);
        let argc = self.compile_call_args(node);
        self.emit(Op::GetLocal, Operand::Slot(t), node);
        self.emit(Op::CallPointer, Operand::Argc(argc), node);
    }

    fn compile_member_read(&mut self, node: &Node<'a>) {
        let Some((base, field)) = self.split_member(node) else {
            self.fail(node, "bad member access".to_string());
            self.emit_simple(Op::GetUndefined, node);
            return;
        };
        if let Some("identifier") = Some(base.kind()) {
            let name = self.text(&base).to_string();
            if is_game_object(&name) {
                self.emit(
                    Op::NeedGame,
                    Operand::Name(format!("{name} needs a game attached")),
                    node,
                );
                self.emit_simple(Op::GetUndefined, node);
                return;
            }
        }
        self.compile_expr(&base);
        self.emit(Op::GetField, Operand::Name(field), node);
    }

    fn compile_array_read(&mut self, node: &Node<'a>) {
        let mut kids = self.named_children(node);
        if kids.is_empty() {
            self.fail(node, "bad array access".to_string());
            self.emit_simple(Op::GetUndefined, node);
            return;
        }
        let base = self.peel(kids.remove(0));
        if base.kind() == "identifier" {
            let name = self.text(&base).to_string();
            if is_game_object(&name) {
                self.emit(
                    Op::NeedGame,
                    Operand::Name(format!("{name} needs a game attached")),
                    node,
                );
                self.emit_simple(Op::GetUndefined, node);
                return;
            }
        }
        self.compile_expr(&base);
        if kids.is_empty() {
            self.fail(node, "bad array access".to_string());
            return;
        }
        for k in kids {
            let idx = self.peel(k);
            self.check_index(&idx);
            self.compile_expr(&idx);
            self.emit_simple(Op::GetIndex, node);
        }
    }
}
