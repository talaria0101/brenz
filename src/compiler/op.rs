//! Bytecode instructions, mirroring coduomp's `script_interpreter_opcode_e`.
//!
//! Opcodes keep their engine numbers where they exist, so behavior can be
//! checked against `script_vm.cpp` case by case. Engine-only operations
//! (entities, threads, notifications) compile to `NeedGame`, which raises
//! a clear runtime error only if executed.

// Execution lives here; the language server only runs it in tests
// for now, so silence reachability noise until an LSP caller lands.
#![allow(dead_code)]
/// An instruction. Jumps carry absolute code offsets, patched by the
/// compiler after the target is emitted.
#[derive(Debug, Clone)]
pub(crate) struct Instr {
    pub op: Op,
    pub arg: Operand,
    /// Byte offset of the source node that produced this instruction.
    pub pos: usize,
}

#[derive(Debug, Clone)]
pub(crate) enum Op {
    End = 0x00,
    Return = 0x01,
    GetUndefined = 0x02,
    GetInt = 0x03,
    GetFloat = 0x04,
    GetString = 0x05,
    GetIStr = 0x06,
    GetLocal = 0x11,
    /// Pops the value into the slot; assignment expressions `Dup` first
    /// so the assignment still yields its value.
    SetLocal = 0x1c,
    NewArray = 0x17,
    GetIndex = 0x14,
    SetIndex = 0x15,
    GetField = 0x18,
    SetField = 0x19,
    GetFuncRef = 0x10,
    CallPointer = 0x26,
    BuiltinFunction = 0x21,
    CallFunction = 0x25,
    CallMethod = 0x27,
    DropTop = 0x2d,
    CastBool = 0x2f,
    CastInt = 0x30,
    CastFloat = 0x31,
    CastString = 0x32,
    BoolNot = 0x33,
    BitNot = 0x34,
    JumpOnFalse = 0x35,
    JumpOnTrue = 0x36,
    Jump = 0x39,
    JumpBack = 0x3a,
    Inc = 0x3b,
    Dec = 0x3c,
    BitOr = 0x3d,
    BitXor = 0x3e,
    BitAnd = 0x3f,
    Equal = 0x40,
    NotEqual = 0x41,
    Less = 0x42,
    Greater = 0x43,
    LessEqual = 0x44,
    GreaterEqual = 0x45,
    ShiftLeft = 0x46,
    ShiftRight = 0x47,
    Plus = 0x48,
    Minus = 0x49,
    Multiply = 0x4a,
    Divide = 0x4b,
    Modulo = 0x4c,
    Size = 0x4d,
    Vector = 0x55,
    Nop = 0x56,
    /// Engine operation with no game attached (entities, `wait`,
    /// `thread`, `notify`, foreign calls). The argument carries the
    /// message raised if execution reaches it.
    NeedGame = 0x57,
    /// Compiler-internal duplicate-top-of-stack. No engine equivalent;
    /// the engine keeps such temporaries in hidden slots instead.
    Dup = 0x60,
    GetGlobal = 0x61,
    SetGlobal = 0x62,
}

/// Immediate operand attached to an instruction.
#[derive(Debug, Clone, Default)]
pub(crate) enum Operand {
    #[default]
    None,
    Int(i32),
    Float(f32),
    Str(String),
    Slot(u32),
    Addr(usize),
    Name(String),
    /// Builtin call with a fixed argument count known at compile time.
    FuncCall { name: String, argc: usize },
    /// Dynamic pointer call with the written argument count.
    Argc(usize),
}

impl Instr {
    pub(crate) fn new(op: Op, arg: Operand, pos: usize) -> Self {
        Self { op, arg, pos }
    }
}
