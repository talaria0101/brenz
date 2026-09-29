//! Stack VM executing [`Program`] code.
//!
//! One shared instruction stream, a value stack, and a frame per call,
//! like `VM_Execute` in coduomp: locals live in the frame, calls bind
//! parameters positionally, missing arguments arrive as `undefined`,
//! extras are dropped, and depth past 32 aborts. An instruction fuel
//! limit (configurable) stops infinite loops, which have no game
//! scheduler to preempt them here.

use std::cell::RefCell;
use std::rc::Rc;

use super::builtin::{self, BuiltinError, Rng};
use super::compile::Program;
use super::op::{Op, Operand};
use super::value::Value;

/// Maximum call depth, like `SCRIPT_INTERPRETER_MAX_CALL_DEPTH`.
pub(crate) const MAX_CALL_DEPTH: usize = 32;

/// Default instruction fuel per run.
pub(crate) const DEFAULT_FUEL: u64 = 1_000_000;

/// A runtime failure: source byte offset plus message.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RuntimeError {
    pub pos: usize,
    pub message: String,
}

/// Successful run: the entry function's return plus captured output.
#[derive(Debug, Clone, Default)]
pub(crate) struct RunResult {
    pub value: Value,
    pub output: Vec<String>,
}

impl Default for Value {
    fn default() -> Self {
        Value::Undefined
    }
}

struct Frame {
    locals: Vec<Value>,
    self_val: Value,
}

pub(crate) struct Vm<'a> {
    program: &'a Program,
    stack: Vec<Value>,
    frames: Vec<Frame>,
    globals: Vec<Value>,
    output: Vec<String>,
    rng: Rng,
    fuel: u64,
}

impl<'a> Vm<'a> {
    pub(crate) fn new(program: &'a Program, seed: u64, fuel: u64) -> Self {
        Self {
            program,
            stack: Vec::new(),
            frames: Vec::new(),
            globals: vec![Value::Undefined; program.global_slots],
            output: Vec::new(),
            rng: Rng(seed),
            fuel,
        }
    }

    /// Run `__globals` once, then call `entry` with `args`.
    pub(crate) fn run(&mut self, entry: &str, args: Vec<Value>) -> Result<RunResult, RuntimeError> {
        if self.program.functions.contains_key("__globals") {
            self.call_function("__globals", Value::Undefined, Vec::new())?;
            self.stack.pop();
        }
        let value = self.call_function(entry, Value::Undefined, args)?;
        Ok(RunResult {
            value,
            output: std::mem::take(&mut self.output),
        })
    }

    fn call_function(
        &mut self,
        name: &str,
        self_val: Value,
        args: Vec<Value>,
    ) -> Result<Value, RuntimeError> {
        let info = self.program.functions.get(name).cloned().ok_or_else(|| RuntimeError {
            pos: 0,
            message: format!("unknown function '{name}'"),
        })?;
        if self.frames.len() >= MAX_CALL_DEPTH {
            return Err(RuntimeError {
                pos: 0,
                message: "maximum call depth exceeded".to_string(),
            });
        }
        let mut locals = vec![Value::Undefined; info.slots];
        for (i, a) in args.into_iter().enumerate().take(info.params.len()) {
            locals[i] = a;
        }
        self.frames.push(Frame { locals, self_val });
        self.execute(info.entry)
    }

    fn frame(&self) -> &Frame {
        self.frames.last().expect("no frame")
    }

    fn frame_mut(&mut self) -> &mut Frame {
        self.frames.last_mut().expect("no frame")
    }

    fn pop(&mut self, pos: usize) -> Result<Value, RuntimeError> {
        self.stack.pop().ok_or(RuntimeError {
            pos,
            message: "stack underflow".to_string(),
        })
    }

    /// Pop exactly the written argument values, in call order. The
    /// compiler guarantees their count, so caller temporaries are
    /// never disturbed.
    fn pop_args(&mut self, pos: usize, argc: usize) -> Result<Vec<Value>, RuntimeError> {
        let mut args = Vec::with_capacity(argc);
        for _ in 0..argc {
            args.push(self.pop(pos)?);
        }
        args.reverse();
        Ok(args)
    }

    fn execute(&mut self, entry: usize) -> Result<Value, RuntimeError> {
        let mut pc = entry;
        loop {
            if self.fuel == 0 {
                return Err(RuntimeError {
                    pos: self.program.code.get(pc).map(|i| i.pos).unwrap_or(0),
                    message: "fuel exhausted (infinite loop?)".to_string(),
                });
            }
            self.fuel -= 1;
            let instr = self.program.code.get(pc).cloned().ok_or(RuntimeError {
                pos: 0,
                message: "jumped out of code".to_string(),
            })?;
            let pos = instr.pos;
            let fail = |message: String| RuntimeError { pos, message };
            match instr.op {
                Op::Nop | Op::End => {}
                Op::Return => {
                    let value = self.pop(pos).unwrap_or(Value::Undefined);
                    self.frames.pop();
                    return Ok(value);
                }
                Op::GetUndefined => self.stack.push(Value::Undefined),
                Op::GetInt => {
                    let Operand::Int(i) = instr.arg else {
                        unreachable!()
                    };
                    self.stack.push(Value::Int(i));
                }
                Op::GetFloat => {
                    let Operand::Float(f) = instr.arg else {
                        unreachable!()
                    };
                    self.stack.push(Value::Float(f));
                }
                Op::GetString => {
                    let Operand::Str(s) = instr.arg else {
                        unreachable!()
                    };
                    self.stack.push(Value::Str(s));
                }
                Op::GetIStr => {
                    let Operand::Str(s) = instr.arg else {
                        unreachable!()
                    };
                    self.stack.push(Value::IStr(s));
                }
                Op::Dup => {
                    let v = self.stack.last().cloned().ok_or_else(|| fail("stack underflow".into()))?;
                    self.stack.push(v);
                }
                Op::DropTop => {
                    self.pop(pos)?;
                }
                Op::GetLocal => {
                    let Operand::Slot(i) = instr.arg else {
                        unreachable!()
                    };
                    let v = self.frame().locals.get(i as usize).cloned().unwrap_or(Value::Undefined);
                    self.stack.push(v);
                }
                Op::SetLocal => {
                    let Operand::Slot(i) = instr.arg else {
                        unreachable!()
                    };
                    let v = self.pop(pos)?;
                    let frame = self.frame_mut();
                    if frame.locals.len() <= i as usize {
                        frame.locals.resize(i as usize + 1, Value::Undefined);
                    }
                    frame.locals[i as usize] = v;
                }
                Op::GetGlobal => {
                    let Operand::Slot(i) = instr.arg else {
                        unreachable!()
                    };
                    let v = self.globals.get(i as usize).cloned().unwrap_or(Value::Undefined);
                    self.stack.push(v);
                }
                Op::SetGlobal => {
                    let Operand::Slot(i) = instr.arg else {
                        unreachable!()
                    };
                    let v = self.pop(pos)?;
                    if self.globals.len() <= i as usize {
                        self.globals.resize(i as usize + 1, Value::Undefined);
                    }
                    self.globals[i as usize] = v;
                }
                Op::NewArray => self.stack.push(Value::Array(Rc::new(RefCell::new(Vec::new())))),
                Op::GetIndex => {
                    let index = self.pop(pos)?;
                    let base = self.pop(pos)?;
                    let Value::Array(items) = base else {
                        return Err(fail(format!("cannot index {}", base.type_name())));
                    };
                    let Value::Int(i) = index else {
                        return Err(fail("array index must be an integer".to_string()));
                    };
                    let v = if i < 0 {
                        Value::Undefined
                    } else {
                        items.borrow().get(i as usize).cloned().unwrap_or(Value::Undefined)
                    };
                    self.stack.push(v);
                }
                Op::SetIndex => {
                    let value = self.pop(pos)?;
                    let index = self.pop(pos)?;
                    let base = self.pop(pos)?;
                    #[cfg(test)]
                    if std::env::var("BRENZ_TRACE").is_ok() {
                        eprintln!("SETINDEX value={value:?} index={index:?} base={base:?}");
                    }
                    let Value::Array(items) = base else {
                        return Err(fail(format!("cannot index {}", base.type_name())));
                    };
                    let Value::Int(i) = index else {
                        return Err(fail("array index must be an integer".to_string()));
                    };
                    if i < 0 {
                        return Err(fail("negative array index".to_string()));
                    }
                    let mut items = items.borrow_mut();
                    if items.len() <= i as usize {
                        items.resize(i as usize + 1, Value::Undefined);
                    }
                    items[i as usize] = value;
                }
                Op::GetField => {
                    let Operand::Name(field) = instr.arg else {
                        unreachable!()
                    };
                    let base = self.pop(pos)?;
                    match base {
                        Value::Struct(fields) => {
                            let v = fields.borrow().get(&field).cloned().unwrap_or(Value::Undefined);
                            self.stack.push(v);
                        }
                        // Idiomatic `a.size`, like the engine's field.
                        Value::Array(items) if field == "size" => {
                            let n = items.borrow().len() as i32;
                            self.stack.push(Value::Int(n));
                        }
                        v => return Err(fail(format!("cannot read field of {}", v.type_name()))),
                    }
                }
                Op::SetField => {
                    let Operand::Name(field) = instr.arg else {
                        unreachable!()
                    };
                    let value = self.pop(pos)?;
                    let base = self.pop(pos)?;
                    let Value::Struct(fields) = base else {
                        return Err(fail(format!("cannot set field of {}", base.type_name())));
                    };
                    fields.borrow_mut().insert(field, value);
                }
                Op::GetFuncRef => {
                    let Operand::Name(name) = instr.arg else {
                        unreachable!()
                    };
                    self.stack.push(Value::Func(name));
                }
                Op::BuiltinFunction => {
                    let Operand::FuncCall { name, argc } = instr.arg else {
                        unreachable!()
                    };
                    let mut args = Vec::new();
                    for _ in 0..argc {
                        args.push(self.pop(pos).unwrap_or(Value::Undefined));
                    }
                    args.reverse();
                    if !builtin::is_pure(&name) {
                        if builtin::is_engine_function(&name) {
                            return Err(fail(format!("{name} needs a game attached")));
                        }
                        return Err(fail(format!("unknown function '{name}'")));
                    }
                    let mut out = std::mem::take(&mut self.output);
                    let result = builtin::call(&name, &args, &mut self.rng, &mut out);
                    self.output = out;
                    match result {
                        Ok(v) => self.stack.push(v),
                        Err(BuiltinError::Script(message)) | Err(BuiltinError::Game(message)) => {
                            return Err(fail(message));
                        }
                    }
                }
                Op::CallFunction => {
                    let Operand::FuncCall { name, argc } = instr.arg else {
                        unreachable!()
                    };
                    let info = self.program.functions.get(&name).cloned();
                    let Some(info) = info else {
                        return Err(fail(format!("unknown function '{name}'")));
                    };
                    let call_args = self.pop_args(pos, argc)?;
                    let value = self.call_with_info(info, Value::Undefined, call_args)?;
                    self.stack.push(value);
                    pc += 1;
                    continue;
                }
                Op::CallMethod => {
                    let Operand::FuncCall { name, argc } = instr.arg else {
                        unreachable!()
                    };
                    let info = self.program.functions.get(&name).cloned();
                    let Some(info) = info else {
                        return Err(fail(format!("unknown function '{name}'")));
                    };
                    let call_args = self.pop_args(pos, argc)?;
                    let self_val = self.pop(pos)?;
                    let value = self.call_with_info(info, self_val, call_args)?;
                    self.stack.push(value);
                    pc += 1;
                    continue;
                }
                Op::CallPointer => {
                    let Operand::Argc(argc) = instr.arg else {
                        unreachable!()
                    };
                    let target = self.pop(pos)?;
                    let Value::Func(name) = target else {
                        return Err(fail("[[...]] called a non-function".to_string()));
                    };
                    let info = self.program.functions.get(&name).cloned();
                    let Some(info) = info else {
                        return Err(fail(format!("unknown function '{name}'")));
                    };
                    // The reference sits below the arguments; pop it after.
                    let call_args = self.pop_args(pos, argc)?;
                    let value = self.call_with_info(info, Value::Undefined, call_args)?;
                    self.stack.push(value);
                    pc += 1;
                    continue;
                }
                Op::CastBool => {
                    let v = self.pop(pos)?;
                    let b = v.cast_bool().map_err(fail)?;
                    self.stack.push(Value::Int(b));
                }
                Op::CastInt => {
                    let v = self.pop(pos)?;
                    let i = v.cast_int().map_err(fail)?;
                    self.stack.push(Value::Int(i));
                }
                Op::CastFloat => {
                    let v = self.pop(pos)?;
                    let f = v.cast_float().map_err(fail)?;
                    self.stack.push(Value::Float(f));
                }
                Op::CastString => {
                    let v = self.pop(pos)?;
                    let s = v.cast_string().map_err(fail)?;
                    self.stack.push(Value::Str(s));
                }
                Op::BoolNot => {
                    let v = self.pop(pos)?;
                    let b = match &v {
                        Value::Int(i) => *i == 0,
                        _ => !v.truthy().map_err(fail)?,
                    };
                    self.stack.push(Value::Int(i32::from(b)));
                }
                Op::BitNot => {
                    let v = self.pop(pos)?;
                    let Value::Int(i) = v else {
                        return Err(fail(format!("~ cannot be applied to \"{}\"", v.type_name())));
                    };
                    self.stack.push(Value::Int(!i));
                }
                Op::JumpOnFalse => {
                    let Operand::Addr(t) = instr.arg else {
                        unreachable!()
                    };
                    let v = self.pop(pos)?;
                    let cond = match &v {
                        Value::Int(i) => *i != 0,
                        _ => v.truthy().map_err(fail)?,
                    };
                    pc = if cond { pc + 1 } else { t };
                    continue;
                }
                Op::JumpOnTrue => {
                    let Operand::Addr(t) = instr.arg else {
                        unreachable!()
                    };
                    let v = self.pop(pos)?;
                    let cond = match &v {
                        Value::Int(i) => *i != 0,
                        _ => v.truthy().map_err(fail)?,
                    };
                    pc = if cond { t } else { pc + 1 };
                    continue;
                }
                Op::Jump => {
                    let Operand::Addr(t) = instr.arg else {
                        unreachable!()
                    };
                    pc = t;
                    continue;
                }
                Op::JumpBack => {
                    let Operand::Addr(t) = instr.arg else {
                        unreachable!()
                    };
                    pc = t;
                    continue;
                }
                Op::Inc | Op::Dec => {
                    let v = self.pop(pos)?;
                    let out = match v {
                        Value::Int(i) => {
                            Value::Int(if matches!(instr.op, Op::Inc) { i.wrapping_add(1) } else { i.wrapping_sub(1) })
                        }
                        Value::Float(f) => {
                            Value::Float(if matches!(instr.op, Op::Inc) { f + 1.0 } else { f - 1.0 })
                        }
                        v => return Err(fail(format!("cannot increment {}", v.type_name()))),
                    };
                    self.stack.push(out);
                }
                Op::BitOr | Op::BitXor | Op::BitAnd => {
                    let b = self.pop(pos)?;
                    let a = self.pop(pos)?;
                    let (Value::Int(x), Value::Int(y)) = (&a, &b) else {
                        return Err(fail(format!(
                            "bitwise op needs integers, got {} and {}",
                            a.type_name(),
                            b.type_name()
                        )));
                    };
                    let r = match instr.op {
                        Op::BitOr => x | y,
                        Op::BitXor => x ^ y,
                        _ => x & y,
                    };
                    self.stack.push(Value::Int(r));
                }
                Op::Equal | Op::NotEqual => {
                    let b = self.pop(pos)?;
                    let a = self.pop(pos)?;
                    let eq = Value::equals(&a, &b).map_err(fail)?;
                    let out = matches!(instr.op, Op::Equal) == eq;
                    self.stack.push(Value::Int(i32::from(out)));
                }
                Op::Less | Op::Greater | Op::LessEqual | Op::GreaterEqual => {
                    let b = self.pop(pos)?;
                    let a = self.pop(pos)?;
                    let name = match instr.op {
                        Op::Less => "<",
                        Op::Greater => ">",
                        Op::LessEqual => "<=",
                        _ => ">=",
                    };
                    let out = Value::compare(name, &a, &b).map_err(fail)?;
                    self.stack.push(Value::Int(i32::from(out)));
                }
                Op::ShiftLeft | Op::ShiftRight => {
                    let b = self.pop(pos)?;
                    let a = self.pop(pos)?;
                    let (Value::Int(x), Value::Int(y)) = (a, b) else {
                        return Err(fail("shift needs integers".to_string()));
                    };
                    let y = (y as u32) & 0x1f;
                    let r = match instr.op {
                        Op::ShiftLeft => x.wrapping_shl(y),
                        _ => x.wrapping_shr(y),
                    };
                    self.stack.push(Value::Int(r));
                }
                Op::Plus | Op::Minus | Op::Multiply | Op::Divide | Op::Modulo => {
                    let b = self.pop(pos)?;
                    let a = self.pop(pos)?;
                    let ch = match instr.op {
                        Op::Plus => '+',
                        Op::Minus => '-',
                        Op::Multiply => '*',
                        Op::Divide => '/',
                        _ => '%',
                    };
                    let out = Value::arith(ch, &a, &b).map_err(fail)?;
                    self.stack.push(out);
                }
                Op::Size => {
                    let v = self.pop(pos)?;
                    let n = v.size().map_err(fail)?;
                    self.stack.push(Value::Int(n));
                }
                Op::Vector => {
                    let z = self.pop(pos)?;
                    let y = self.pop(pos)?;
                    let x = self.pop(pos)?;
                    let c = |v: Value| {
                        v.cast_float()
                            .map_err(|e| RuntimeError { pos, message: e })
                    };
                    self.stack.push(Value::Vec3([c(x)?, c(y)?, c(z)?]));
                }
                Op::NeedGame => {
                    let Operand::Name(message) = instr.arg else {
                        unreachable!()
                    };
                    return Err(fail(message));
                }
            }
            pc += 1;
        }
    }

    /// Shared call helper that preserves the caller's pc across the call.
    /// Written arguments bind positionally; missing ones arrive as
    /// `undefined` and extras are dropped.
    fn call_with_info(
        &mut self,
        info: super::compile::FuncInfo,
        self_val: Value,
        args: Vec<Value>,
    ) -> Result<Value, RuntimeError> {
        if self.frames.len() >= MAX_CALL_DEPTH {
            return Err(RuntimeError {
                pos: 0,
                message: "maximum call depth exceeded".to_string(),
            });
        }
        let mut locals = vec![Value::Undefined; info.slots];
        for (i, a) in args.into_iter().enumerate().take(info.params.len()) {
            locals[i] = a;
        }
        self.frames.push(Frame { locals, self_val });
        self.execute(info.entry)
    }
}
