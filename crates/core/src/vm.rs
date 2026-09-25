//! The register machine that runs a compiled [`Program`].
//!
//! Frames live on one value stack; a call moves its arguments into the new
//! frame's first registers. Errors, and `반복을 끝내자` outside any loop of
//! its function, unwind frames until a handler or a loop takes them (Hana
//! lets a break in a function end the caller's loop).

use std::collections::HashMap;
use std::io::Write;

use haru_abi::tag;

use crate::builtins;
use crate::bytecode::*;
use crate::error::RuntimeError;
use crate::format::{display, go_int, go_i64};
use crate::lang::Lang;
use crate::value::{FuncObj, Key, Value};

/// Hana's limit on nested calls (`vm.MaxCallDepth`).
pub const MAX_CALL_DEPTH: u32 = 10000;

/// The runtime's error codes (the text is Hana's catalog).
pub mod codes {
    pub const VARIABLE_NOT_FOUND: &str = "ReferenceError.VariableNotFound";
    pub const VARIABLE_TYPE: &str = "TypeError.VariableTypeMismatch";
    pub const ARGUMENT_TYPE: &str = "TypeError.ArgumentTypeMismatch";
    pub const RETURN_TYPE: &str = "TypeError.ReturnTypeMismatch";
    pub const CONSTANT: &str = "ConstantAssignmentError.ConstantAssignment";
    pub const OPERAND_TYPES: &str = "TypeError.OperandTypeMismatch";
    pub const NULL_OPERAND: &str = "NullReferenceError.NullOperand";
    pub const UNKNOWN_OPERATOR: &str = "TypeError.UnknownOperator";
    pub const DIVIDE_BY_ZERO: &str = "DivideByZeroError.DivideByZero";
    pub const NOT_BOOLEAN: &str = "TypeError.ConditionNotBoolean";
    pub const RANGE_NUMBERS: &str = "TypeError.RangeMustBeNumbers";
    pub const NOT_ITERABLE: &str = "TypeError.NotIterable";
    pub const LIST_INDEX_RANGE: &str = "IndexOutOfBoundsError.ListIndexOutOfRange";
    pub const LIST_INDEX_NUMBER: &str = "TypeError.ListIndexMustBeNumber";
    pub const STRING_INDEX_RANGE: &str = "IndexOutOfBoundsError.StringIndexOutOfRange";
    pub const MEMBER_UNSUPPORTED: &str = "MemberAccessError.MemberAccessUnsupported";
    pub const MEMBER_ON_STRING: &str = "MemberAccessError.MemberAccessOnString";
    pub const DICT_KEY: &str = "KeyError.DictKeyNotFound";
    pub const STRING_INDEX_ASSIGN: &str = "ImmutableAssignmentError.StringIndex";
    pub const NOT_A_LIST: &str = "TypeError.NotAList";
    pub const LIST_EMPTY: &str = "IndexOutOfBoundsError.ListEmpty";
    pub const TOO_MANY_ARGUMENTS: &str = "ArgumentError.TooManyArguments";
    pub const MISSING_ARGUMENT: &str = "MissingArgumentError.MissingArgument";
    pub const FUNCTION_NOT_FOUND: &str = "MethodNotFoundError.GlobalFunctionNotFound";
    pub const NOT_CALLABLE: &str = "TypeError.NotCallable";
    pub const METHOD_NOT_FOUND: &str = "MethodNotFoundError.MethodNotFound";
    pub const ARG_COUNT: &str = "ArgumentError.ArgCountExact";
    pub const METHOD_ARG_NUMBER: &str = "TypeError.MethodArgMustBeNumber";
    pub const METHOD_ARG_STRING: &str = "TypeError.MethodArgMustBeString";
    pub const CALL_TOO_DEEP: &str = "RecursionError.CallTooDeep";
    pub const TO_NUMBER_FAILED: &str = "ConversionError.ConvertToNumberFailed";
    pub const TO_NUMBER_INVALID: &str = "ConversionError.ConvertToNumberInvalid";
    pub const TO_CODE: &str = "ConversionError.ConvertToCodeNeedsOneChar";
    pub const TO_TEXT: &str = "ConversionError.ConvertToTextNeedsNumber";
    pub const INPUT_NUMBER: &str = "InputConversionError.InputToNumberFailed";
    pub const INPUT_BOOLEAN: &str = "InputConversionError.InputToBooleanFailed";
    pub const INPUT_TYPE: &str = "UnsupportedInputTypeError.InputTypeUnsupported";

    /// Not Hana's: a construct this version cannot run yet.
    pub const UNSUPPORTED: &str = "Unsupported";
}

use codes::*;

fn err(code: &str) -> RuntimeError {
    RuntimeError::core(code)
}

/// Why execution left the normal path.
#[derive(Debug, Clone)]
pub enum Signal {
    Error(RuntimeError),
    /// `반복을 끝내자` looking for a loop.
    Break,
}

impl From<RuntimeError> for Signal {
    fn from(e: RuntimeError) -> Signal {
        Signal::Error(e)
    }
}

type Flow<T> = Result<T, Signal>;

struct Frame {
    proto: u32,
    pc: usize,
    base: usize,
    argc: u16,
    /// Call nesting when the frame started (see `Op::Enter`).
    depth: u32,
    /// The caller's register for the result.
    ret: Reg,
    /// The error a handler was entered for.
    pending: Option<Box<Signal>>,
}

/// Where printed text goes.
pub enum Output {
    Stdout(Vec<u8>),
    Capture(String),
}

pub struct Vm<'p> {
    prog: &'p Program,
    lang: &'static Lang,
    globals: Vec<Value>,
    stack: Vec<Value>,
    frames: Vec<Frame>,
    depth: u32,
    global_by_name: HashMap<&'p str, u32>,
    function_by_name: HashMap<&'p str, u32>,
    pub output: Output,
    last_flush: std::time::Instant,
    /// The next line of input; `None` at the end (reads as an empty line).
    pub read_line: Box<dyn FnMut() -> Option<String>>,
}

impl<'p> Vm<'p> {
    pub fn new(prog: &'p Program) -> Vm<'p> {
        Vm {
            prog,
            lang: prog.lang,
            globals: prog.globals.clone(),
            stack: Vec::with_capacity(1024),
            frames: Vec::new(),
            depth: 0,
            global_by_name: prog.global_names.iter().map(|(n, g)| (n.as_str(), *g)).collect(),
            function_by_name: prog.functions.iter().map(|(n, p)| (n.as_str(), *p)).collect(),
            output: Output::Stdout(Vec::new()),
            last_flush: std::time::Instant::now(),
            read_line: Box::new(|| None),
        }
    }

    pub fn flush(&mut self) {
        if let Output::Stdout(buf) = &mut self.output {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(buf);
            let _ = out.flush();
            buf.clear();
        }
        self.last_flush = std::time::Instant::now();
    }

    fn print(&mut self, text: &str, newline: bool) {
        match &mut self.output {
            Output::Stdout(buf) => {
                buf.extend_from_slice(text.as_bytes());
                if newline {
                    buf.push(b'\n');
                }
                // Like Hana's console: written out at 32KB or after 25ms.
                if buf.len() > 32 * 1024 || self.last_flush.elapsed().as_millis() >= 25 {
                    self.flush();
                }
            }
            Output::Capture(s) => {
                s.push_str(text);
                if newline {
                    s.push('\n');
                }
            }
        }
    }

    /// Runs the program to its end.
    pub fn run(&mut self) -> Result<(), RuntimeError> {
        let main = &self.prog.protos[0];
        self.stack.resize(main.nregs as usize, Value::UNDEF);
        self.frames.push(Frame { proto: 0, pc: 0, base: 0, argc: 0, depth: 0, ret: 0, pending: None });
        let result = loop {
            match self.exec() {
                Ok(()) => break Ok(()),
                Err(signal) => {
                    if let Err(e) = self.unwind(signal) {
                        break Err(e);
                    }
                }
            }
        };
        self.flush();
        result
    }

    /// Hands a signal to the nearest handler or loop, leaving frames that
    /// have none. Fails when it reaches the top.
    fn unwind(&mut self, signal: Signal) -> Result<(), RuntimeError> {
        loop {
            let frame = self.frames.last_mut().unwrap();
            let proto = &self.prog.protos[frame.proto as usize];
            let at = frame.pc.saturating_sub(1) as u32;
            // Handlers guard expressions, so any containing `at` is innermost.
            if let Some(h) = proto.handlers.iter().filter(|h| h.start <= at && at < h.end).min_by_key(|h| h.end - h.start) {
                frame.pc = h.target as usize;
                frame.pending = Some(Box::new(signal));
                self.depth = frame.depth + h.open_calls;
                return Ok(());
            }
            if let Signal::Break = signal {
                if let Some(l) = proto.loops.iter().filter(|l| l.start <= at && at < l.end).min_by_key(|l| l.end - l.start) {
                    frame.pc = l.exit as usize;
                    self.depth = frame.depth;
                    return Ok(());
                }
            }
            if self.frames.len() == 1 {
                return Err(match signal {
                    Signal::Error(e) => e,
                    Signal::Break => err("break"),
                });
            }
            // Carry on from the caller's call instruction.
            let frame = self.frames.pop().unwrap();
            self.stack.truncate(frame.base);
        }
    }

    // ---- variables

    fn load(&self, loc: Loc, base: usize) -> &Value {
        match loc {
            Loc::Reg(r) => &self.stack[base + r as usize],
            Loc::Global(g) => &self.globals[g as usize],
        }
    }

    fn store(&mut self, loc: Loc, base: usize, v: Value) {
        match loc {
            Loc::Reg(r) => self.stack[base + r as usize] = v,
            Loc::Global(g) => self.globals[g as usize] = v,
        }
    }

    /// The first slot of the chain that holds a variable.
    fn find(&self, var: &Var, base: usize) -> Option<Slot> {
        var.slots.iter().copied().find(|s| !self.load(s.loc, base).is_undef())
    }

    fn name(&self, id: u32) -> &'p str {
        &self.prog.names[id as usize]
    }

    fn not_found(&self, name: u32) -> Signal {
        err(VARIABLE_NOT_FOUND).str_arg(self.name(name)).into()
    }

    fn meta(&self, slot: Slot, base: usize) -> (u32, bool) {
        match slot.meta {
            Some(m) => meta_parts(self.load(m, base)),
            None => (0, false),
        }
    }

    /// `정하자` (Hana's `assignVariable`).
    fn declare(&mut self, var_id: u32, base: usize, val: Value, ty: u32, konst: bool) -> Flow<()> {
        let prog = self.prog;
        let var = &prog.vars[var_id as usize];
        if ty != 0 {
            self.check_type(VARIABLE_TYPE, self.name(var.name), ty, &val)?;
        }
        match self.find(var, base) {
            Some(slot) => {
                let (declared, constant) = self.meta(slot, base);
                if declared != 0 && declared != ty {
                    self.check_type(VARIABLE_TYPE, self.name(var.name), declared, &val)?;
                }
                if constant {
                    return Err(err(CONSTANT).str_arg(self.name(var.name)).into());
                }
                self.store(slot.loc, base, val);
            }
            None => {
                let slot = var.slots[0];
                self.store(slot.loc, base, val);
                if let Some(m) = slot.meta {
                    self.store(m, base, meta_value(ty, konst));
                }
            }
        }
        Ok(())
    }

    /// An assignment statement: only an existing variable changes.
    fn assign(&mut self, var_id: u32, base: usize, v: Value) -> Flow<()> {
        let var = &self.prog.vars[var_id as usize];
        if let Some(slot) = self.find(var, base) {
            let (declared, constant) = self.meta(slot, base);
            if declared != 0 {
                self.check_type(VARIABLE_TYPE, self.name(var.name), declared, &v)?;
            }
            if constant {
                return Err(err(CONSTANT).str_arg(self.name(var.name)).into());
            }
            self.store(slot.loc, base, v);
        }
        Ok(())
    }

    fn check_type(&self, code: &str, name: &str, ty: u32, v: &Value) -> Flow<()> {
        let spec = &self.prog.types[ty as usize];
        if self.accepts(spec, v) {
            return Ok(());
        }
        Err(err(code).str_arg(name).str_arg(&spec.text).str_arg(self.describe(v)).into())
    }

    fn accepts(&self, spec: &TypeSpec, v: &Value) -> bool {
        accepts(&spec.kind, v)
    }

    /// A value's type name in the program's language (for messages).
    fn describe(&self, v: &Value) -> &'static str {
        let l = self.lang;
        match v.tag() {
            tag::NULL => l.type_null,
            tag::NUM => l.type_number,
            tag::STR => l.type_string,
            tag::BOOL => l.type_boolean,
            tag::LIST => l.type_list,
            tag::DICT => l.type_dict,
            _ => "?",
        }
    }

    // ---- calls

    fn call_proto(&mut self, proto_id: u32, arg_base: usize, argc: u16, ret: Reg) -> Flow<()> {
        let proto = &self.prog.protos[proto_id as usize];
        let nparams = proto.params.len();
        if argc as usize > nparams {
            return Err(err(TOO_MANY_ARGUMENTS).num_arg(nparams as f64).num_arg(argc as f64).into());
        }
        for (i, p) in proto.params.iter().enumerate().take(argc as usize) {
            if p.ty != 0 {
                self.check_type(ARGUMENT_TYPE, self.name(p.name), p.ty, &self.stack[arg_base + i])?;
            }
        }
        let base = self.stack.len();
        self.stack.resize(base + proto.nregs as usize, Value::UNDEF);
        for (i, p) in proto.params.iter().enumerate().take(argc as usize) {
            let v = std::mem::replace(&mut self.stack[arg_base + i], Value::UNDEF);
            self.stack[base + p.slot as usize] = v;
            if let Some(m) = p.meta {
                self.stack[base + m as usize] = meta_value(p.ty, false);
            }
        }
        self.frames.push(Frame { proto: proto_id, pc: 0, base, argc, depth: self.depth, ret, pending: None });
        Ok(())
    }

    /// Calls a function value. `Ok(Some(v))` for an immediate result;
    /// `Ok(None)` when a frame was pushed.
    fn call_value(&mut self, f: &Value, arg_base: usize, argc: u16, ret: Reg) -> Flow<Option<Value>> {
        match f.as_func() {
            Some(FuncObj::User(p)) => {
                self.call_proto(*p, arg_base, argc, ret)?;
                Ok(None)
            }
            Some(FuncObj::Builtin(b)) => {
                let args: Vec<Value> = (0..argc as usize).map(|i| self.stack[arg_base + i].clone()).collect();
                Ok(Some(builtins::call(*b, &args, self.lang)?))
            }
            Some(FuncObj::Native { .. }) => Err(err(UNSUPPORTED).str_arg("native module call").into()),
            None => match f.as_str() {
                // A name: what it means now (a built-in or function by that name).
                Some(name) => {
                    let name = name.to_string();
                    if let Some(&g) = self.global_by_name.get(name.as_str()) {
                        let v = self.globals[g as usize].clone();
                        if matches!(v.as_func(), Some(FuncObj::Builtin(_) | FuncObj::User(_))) {
                            return self.call_value(&v, arg_base, argc, ret);
                        }
                    }
                    if let Some(&p) = self.function_by_name.get(name.as_str()) {
                        self.call_proto(p, arg_base, argc, ret)?;
                        return Ok(None);
                    }
                    Err(err(FUNCTION_NOT_FOUND).str_arg(&name).into())
                }
                None => Err(err(NOT_CALLABLE).into()),
            },
        }
    }

    // ---- the loop

    fn exec(&mut self) -> Flow<()> {
        let prog = self.prog;
        'frames: loop {
            let fi = self.frames.len() - 1;
            let frame = &self.frames[fi];
            let proto = &prog.protos[frame.proto as usize];
            let code = &proto.code[..];
            let base = frame.base;
            let mut pc = frame.pc;

            // Registers are below the frame's size (the compiler counts them)
            // and the stack holds every frame: no bounds check per access. The
            // stack only grows in a call, which leaves this loop ('frames), so
            // the pointer stays valid while this frame runs.
            let regs: *mut Value = unsafe { self.stack.as_mut_ptr().add(base) };
            macro_rules! reg {
                ($r:expr) => {
                    *{
                        debug_assert!(base + ($r as usize) < self.stack.len());
                        unsafe { &mut *regs.add($r as usize) }
                    }
                };
            }
            macro_rules! fail {
                ($e:expr) => {{
                    self.frames[fi].pc = pc;
                    return Err($e.into());
                }};
            }
            macro_rules! tri {
                ($e:expr) => {
                    match $e {
                        Ok(v) => v,
                        Err(e) => fail!(e),
                    }
                };
            }

            loop {
                debug_assert!(pc < code.len());
                // Every function ends in a return, so pc stays inside the code.
                let op = unsafe { *code.get_unchecked(pc) };
                pc += 1;
                match op {
                    Op::LoadK { dst, k } => reg!(dst) = prog.consts[k as usize].clone(),
                    Op::LoadNull { dst } => reg!(dst) = Value::NULL,
                    Op::LoadBool { dst, v } => reg!(dst) = Value::bool(v),
                    Op::Move { dst, src } => reg!(dst) = reg!(src).clone(),

                    Op::GetReg { dst, slot, name } => {
                        let v = &reg!(slot);
                        if v.is_undef() {
                            fail!(self.not_found(name));
                        }
                        reg!(dst) = v.clone();
                    }
                    Op::GetGlobal { dst, slot, name } => {
                        let v = &self.globals[slot as usize];
                        if v.is_undef() {
                            fail!(self.not_found(name));
                        }
                        reg!(dst) = v.clone();
                    }
                    Op::GetVar { dst, var } => {
                        let var = &prog.vars[var as usize];
                        match self.find(var, base) {
                            Some(s) => reg!(dst) = self.load(s.loc, base).clone(),
                            None => fail!(self.not_found(var.name)),
                        }
                    }
                    Op::Decl { var, src, ty, konst } => {
                        let v = std::mem::replace(&mut reg!(src), Value::UNDEF);
                        tri!(self.declare(var, base, v, ty, konst));
                    }
                    Op::SetReg { slot, src } => reg!(slot) = std::mem::replace(&mut reg!(src), Value::UNDEF),
                    Op::SetGlobal { slot, src } => {
                        self.globals[slot as usize] = std::mem::replace(&mut reg!(src), Value::UNDEF);
                    }
                    Op::Assign { var, src } => {
                        let v = std::mem::replace(&mut reg!(src), Value::UNDEF);
                        tri!(self.assign(var, base, v));
                    }
                    Op::Update { var, a, b, op } => {
                        if op == BinOp::Add && reg!(a).tag() == tag::STR && reg!(b).tag() == tag::STR {
                            let v = &prog.vars[var as usize];
                            if let Some(slot) = self.find(v, base) {
                                if self.load(slot.loc, base).same_object(&reg!(a)) {
                                    // The result is a string: check it as the value read.
                                    let (declared, constant) = self.meta(slot, base);
                                    if declared != 0 {
                                        tri!(self.check_type(VARIABLE_TYPE, self.name(v.name), declared, &reg!(a)));
                                    }
                                    if constant {
                                        fail!(err(CONSTANT).str_arg(self.name(v.name)));
                                    }
                                    reg!(a) = Value::UNDEF;
                                    let piece = std::mem::replace(&mut reg!(b), Value::UNDEF);
                                    let target = match slot.loc {
                                        Loc::Reg(r) => &mut self.stack[base + r as usize],
                                        Loc::Global(g) => &mut self.globals[g as usize],
                                    };
                                    let piece = piece.as_str().unwrap();
                                    if !target.append_in_place(piece) {
                                        let joined = format!("{}{piece}", target.as_str().unwrap());
                                        *target = Value::string(joined);
                                    }
                                    continue;
                                }
                            }
                        }
                        let result = match (reg!(a).as_num(), reg!(b).as_num(), op) {
                            (Some(x), Some(y), BinOp::Add) => boxed(x + y),
                            (Some(x), Some(y), BinOp::Sub) => boxed(x - y),
                            _ => tri!(self.slow_binary(op, &reg!(a), &reg!(b))),
                        };
                        tri!(self.assign(var, base, result));
                    }
                    Op::Undef { from, to } => {
                        for r in from..to {
                            reg!(r) = Value::UNDEF;
                        }
                    }

                    Op::BinK { op, dst, a, k } => {
                        let x = &reg!(a);
                        let y = &prog.consts[k as usize];
                        let v = match (x.as_num(), y.as_num()) {
                            (Some(x), Some(y)) => tri!(arith(op, x, y)),
                            _ => tri!(self.slow_binary(op, x, y)),
                        };
                        reg!(dst) = v;
                    }
                    Op::Bin { op, dst, a, b } => {
                        let (x, y) = (&reg!(a), &reg!(b));
                        let v = match (x.as_num(), y.as_num()) {
                            (Some(x), Some(y)) => match op {
                                BinOp::Add => boxed(x + y),
                                BinOp::Sub => boxed(x - y),
                                BinOp::Mul => boxed(x * y),
                                BinOp::Div => {
                                    if y == 0.0 {
                                        fail!(err(DIVIDE_BY_ZERO));
                                    }
                                    boxed(x / y)
                                }
                                BinOp::Mod => {
                                    let d = go_i64(y);
                                    if d == 0 {
                                        fail!(err(DIVIDE_BY_ZERO));
                                    }
                                    boxed(go_i64(x).wrapping_rem(d) as f64)
                                }
                                BinOp::Gt => Value::bool(x > y),
                                BinOp::Lt => Value::bool(x < y),
                                BinOp::Ge => Value::bool(x >= y),
                                BinOp::Le => Value::bool(x <= y),
                            },
                            _ => tri!(self.slow_binary(op, x, y)),
                        };
                        reg!(dst) = v;
                    }
                    Op::Eq { dst, a, b, neg } => {
                        let eq = reg!(a).go_eq(&reg!(b));
                        reg!(dst) = Value::bool(eq != neg);
                    }
                    Op::Truth { dst, src } => {
                        let v = &reg!(src);
                        match v.as_bool() {
                            Some(b) => reg!(dst) = Value::bool(b),
                            None => fail!(err(NOT_BOOLEAN).str_arg(self.describe(v))),
                        }
                    }
                    Op::UnknownOp { k } => fail!(err(UNKNOWN_OPERATOR).arg(prog.consts[k as usize].clone())),
                    Op::Jump { to } => pc = to as usize,
                    Op::JumpIfFalse { cond, to } => {
                        let v = &reg!(cond);
                        match v.as_bool() {
                            Some(false) => pc = to as usize,
                            Some(true) => {}
                            None => fail!(err(NOT_BOOLEAN).str_arg(self.describe(v))),
                        }
                    }
                    Op::JumpIfTrue { cond, to } => {
                        let v = &reg!(cond);
                        match v.as_bool() {
                            Some(true) => pc = to as usize,
                            Some(false) => {}
                            None => fail!(err(NOT_BOOLEAN).str_arg(self.describe(v))),
                        }
                    }

                    Op::RangePrep { start, end, step } => {
                        let (Some(s), Some(e)) = (reg!(start).as_num(), reg!(end).as_num()) else {
                            fail!(err(RANGE_NUMBERS));
                        };
                        reg!(step) = Value::num(if s > e { -1.0 } else { 1.0 });
                    }
                    Op::RangeTest { v, end, step, exit } => {
                        let (v, e, s) = (reg!(v).as_num().unwrap(), reg!(end).as_num().unwrap(), reg!(step).as_num().unwrap());
                        if (s > 0.0 && v > e) || (s < 0.0 && v < e) {
                            pc = exit as usize;
                        }
                    }
                    Op::RangeStep { v, step } => {
                        let n = reg!(v).as_num().unwrap() + reg!(step).as_num().unwrap();
                        reg!(v) = Value::num(n);
                    }
                    Op::Boxed { dst } => {
                        if let Some(n) = reg!(dst).as_num() {
                            reg!(dst) = boxed(n);
                        }
                    }
                    Op::IterPrep { dst, src } => {
                        let v = &reg!(src);
                        let items = if let Some(s) = v.as_str() {
                            s.chars().map(|c| Value::string(c.to_string())).collect()
                        } else if let Some(l) = v.as_list() {
                            l.items.borrow().clone()
                        } else {
                            fail!(err(NOT_ITERABLE));
                        };
                        reg!(dst) = Value::list(items);
                    }
                    Op::IterNext { iter, idx, dst, exit } => {
                        let i = reg!(idx).as_num().unwrap() as usize;
                        let next = reg!(iter).as_list().unwrap().items.borrow().get(i).cloned();
                        match next {
                            Some(v) => {
                                reg!(dst) = v;
                                reg!(idx) = Value::num((i + 1) as f64);
                            }
                            None => pc = exit as usize,
                        }
                    }

                    Op::Print { src, newline } => {
                        let text = display(&reg!(src), self.lang);
                        self.print(&text, newline);
                    }
                    Op::Input { var, ty } => {
                        self.flush();
                        let line = (self.read_line)().unwrap_or_default();
                        let v = tri!(self.parse_input(ty, &line));
                        if var != NONE {
                            tri!(self.declare(var, base, v, 0, false));
                        }
                    }

                    Op::Enter => {
                        self.depth += 1;
                        if self.depth > MAX_CALL_DEPTH {
                            self.depth -= 1;
                            fail!(err(CALL_TOO_DEEP).num_arg(MAX_CALL_DEPTH as f64));
                        }
                    }
                    Op::Call { dst, proto, base: b, argc } => {
                        self.frames[fi].pc = pc;
                        tri!(self.call_proto(proto, base + b as usize, argc, dst));
                        continue 'frames;
                    }
                    Op::CallName { dst, name, var, base: b, argc } => {
                        let f = if var == NONE {
                            None
                        } else {
                            self.find(&prog.vars[var as usize], base).map(|s| self.load(s.loc, base).clone())
                        };
                        match f {
                            Some(f) if matches!(f.as_func(), Some(FuncObj::Builtin(_) | FuncObj::User(_))) => {
                                self.frames[fi].pc = pc;
                                match tri!(self.call_value(&f, base + b as usize, argc, dst)) {
                                    Some(v) => {
                                        reg!(dst) = v;
                                        self.depth -= 1;
                                    }
                                    None => continue 'frames,
                                }
                            }
                            _ => fail!(err(FUNCTION_NOT_FOUND).str_arg(self.name(name))),
                        }
                    }
                    Op::CallValue { dst, callee, base: b, argc } => {
                        let f = reg!(callee).clone();
                        self.frames[fi].pc = pc;
                        match tri!(self.call_value(&f, base + b as usize, argc, dst)) {
                            Some(v) => {
                                reg!(dst) = v;
                                self.depth -= 1;
                            }
                            None => continue 'frames,
                        }
                    }
                    Op::CallMethod { dst, obj, name, target, base: b, argc } => {
                        let o = reg!(obj).clone();
                        let args: Vec<Value> = (0..argc as usize).map(|i| reg!(b as usize + i).clone()).collect();
                        let v = tri!(self.call_method(&o, self.name(name), target, base, &args));
                        reg!(dst) = v;
                        self.depth -= 1;
                    }
                    Op::Return { src } => {
                        if fi == 0 {
                            // `돌려주자` outside a function: Hana stops with "return".
                            fail!(err("return"));
                        }
                        let v = std::mem::replace(&mut reg!(src), Value::UNDEF);
                        tri!(self.finish_call(v));
                        continue 'frames;
                    }
                    Op::ReturnNull => {
                        if fi == 0 {
                            return Ok(());
                        }
                        tri!(self.finish_call(Value::NULL));
                        continue 'frames;
                    }
                    Op::Break => {
                        self.frames[fi].pc = pc;
                        return Err(Signal::Break);
                    }

                    Op::ArgGiven { index, skip } => {
                        if index < self.frames[fi].argc {
                            pc = skip as usize;
                        }
                    }
                    Op::BindParam { index, src } => {
                        let p = &proto.params[index as usize];
                        let v = reg!(src).clone();
                        if p.ty != 0 {
                            tri!(self.check_type(ARGUMENT_TYPE, self.name(p.name), p.ty, &v));
                        }
                        reg!(p.slot) = v;
                        if let Some(m) = p.meta {
                            reg!(m) = meta_value(p.ty, false);
                        }
                    }
                    Op::MissingArg { index } => {
                        fail!(err(MISSING_ARGUMENT).str_arg(self.name(proto.params[index as usize].name)))
                    }

                    Op::MakeList { dst, base: b, n } => {
                        let items = (0..n as usize).map(|i| std::mem::replace(&mut reg!(b as usize + i), Value::UNDEF)).collect();
                        reg!(dst) = Value::list(items);
                    }
                    Op::MakeDict { dst, base: b, n } => {
                        let mut map = HashMap::with_capacity(n as usize);
                        for i in 0..n as usize {
                            let k = std::mem::replace(&mut reg!(b as usize + 2 * i), Value::UNDEF);
                            let v = std::mem::replace(&mut reg!(b as usize + 2 * i + 1), Value::UNDEF);
                            match Key::new(k) {
                                Some(k) => {
                                    map.insert(k, v);
                                }
                                None => fail!(err(UNSUPPORTED).str_arg("dictionary as a key")),
                            }
                        }
                        reg!(dst) = Value::dict(map);
                    }
                    Op::Member { dst, obj, name, skip } => {
                        let o = &reg!(obj);
                        let length = name != NONE && self.name(name) == self.lang.length_word;
                        match o.tag() {
                            tag::LIST if length => {
                                let n = o.as_list().unwrap().items.borrow().len();
                                reg!(dst) = Value::num(n as f64);
                                pc = skip as usize;
                            }
                            tag::STR if length => {
                                let n = o.as_str().unwrap().chars().count();
                                reg!(dst) = Value::num(n as f64);
                                pc = skip as usize;
                            }
                            tag::LIST | tag::STR | tag::DICT => {}
                            _ => fail!(err(MEMBER_UNSUPPORTED).str_arg(type_name_of(o))),
                        }
                    }
                    Op::Index { dst, obj, key } => {
                        let v = tri!(self.index(&reg!(obj), &reg!(key)));
                        reg!(dst) = v;
                    }
                    Op::IndexFail { obj } => {
                        let pending = self.frames[fi].pending.take();
                        match reg!(obj).tag() {
                            tag::LIST => fail!(err(LIST_INDEX_NUMBER)),
                            tag::STR => fail!(err(MEMBER_ON_STRING)),
                            _ => {
                                self.frames[fi].pc = pc;
                                return Err(pending.map_or(Signal::Error(err(UNSUPPORTED)), |p| *p));
                            }
                        }
                    }
                    Op::SetMember { obj, skip } => match reg!(obj).tag() {
                        tag::STR => fail!(err(STRING_INDEX_ASSIGN)),
                        tag::LIST | tag::DICT => {}
                        _ => pc = skip as usize,
                    },
                    Op::SetIndex { obj, key, val } => {
                        let (o, k, v) = (reg!(obj).clone(), reg!(key).clone(), reg!(val).clone());
                        if let Some(list) = o.as_list() {
                            if let Some(n) = k.as_num() {
                                let i = go_int(n) - 1;
                                let mut items = list.items.borrow_mut();
                                if i >= 0 && (i as usize) < items.len() {
                                    items[i as usize] = v;
                                }
                            }
                        } else if let Some(d) = o.as_dict() {
                            match Key::new(k) {
                                Some(k) => {
                                    d.map.borrow_mut().insert(k, v);
                                }
                                None => fail!(err(UNSUPPORTED).str_arg("dictionary as a key")),
                            }
                        }
                    }
                    Op::SetIndexFail { obj, val, name, skip } => {
                        self.frames[fi].pending = None;
                        if let Some(d) = reg!(obj).as_dict() {
                            if name != NONE {
                                d.map.borrow_mut().insert(Key(Value::str(self.name(name))), reg!(val).clone());
                            }
                        }
                        pc = skip as usize;
                    }
                    Op::ListCheck { list, target } => {
                        if reg!(list).tag() != tag::LIST {
                            fail!(err(NOT_A_LIST));
                        }
                        tri!(self.require_mutable(target, base));
                    }
                    Op::ListPush { list, val, front, target } => {
                        let l = reg!(list).clone();
                        let v = std::mem::replace(&mut reg!(val), Value::UNDEF);
                        let items = &l.as_list().unwrap().items;
                        if front {
                            items.borrow_mut().insert(0, v);
                        } else {
                            items.borrow_mut().push(v);
                        }
                        if let Err(e) = self.check_push(target, base, &l, front) {
                            if front {
                                items.borrow_mut().remove(0);
                            } else {
                                items.borrow_mut().pop();
                            }
                            fail!(e);
                        }
                    }
                    Op::ListPop { dst, list, front } => {
                        let l = reg!(list).clone();
                        let mut items = l.as_list().unwrap().items.borrow_mut();
                        if items.is_empty() {
                            drop(items);
                            fail!(err(LIST_EMPTY));
                        }
                        let v = if front { items.remove(0) } else { items.pop().unwrap() };
                        drop(items);
                        reg!(dst) = v;
                    }

                    Op::Format { dst, src } => {
                        let text = display(&reg!(src), self.lang);
                        reg!(dst) = Value::string(text);
                    }
                    Op::Concat { dst, base: b, n } => {
                        let mut s = String::new();
                        for i in 0..n as usize {
                            s.push_str(reg!(b as usize + i).as_str().unwrap_or(""));
                        }
                        reg!(dst) = Value::string(s);
                    }
                    Op::FuncRef { dst, name, var } => {
                        let n = self.name(name);
                        let v = if let Some(&p) = self.function_by_name.get(n) {
                            Value::func(FuncObj::User(p))
                        } else {
                            match self.find(&prog.vars[var as usize], base) {
                                Some(s) if matches!(self.load(s.loc, base).as_func(), Some(FuncObj::User(_))) => {
                                    self.load(s.loc, base).clone()
                                }
                                _ => Value::str(n),
                            }
                        };
                        reg!(dst) = v;
                    }
                    Op::Unsupported { k } => fail!(err(UNSUPPORTED).arg(prog.consts[k as usize].clone())),
                }
            }
        }
    }

    /// Returns from the running function with `v`.
    fn finish_call(&mut self, v: Value) -> Flow<()> {
        let frame = self.frames.last().unwrap();
        let proto = &self.prog.protos[frame.proto as usize];
        if proto.return_type != 0 {
            self.check_type(RETURN_TYPE, &proto.name, proto.return_type, &v)?;
        }
        let frame = self.frames.pop().unwrap();
        self.stack.truncate(frame.base);
        let caller = self.frames.last().unwrap();
        self.stack[caller.base + frame.ret as usize] = v;
        self.depth -= 1;
        Ok(())
    }

    fn slow_binary(&self, op: BinOp, x: &Value, y: &Value) -> Result<Value, RuntimeError> {
        if x.is_null() || y.is_null() {
            return Err(err(NULL_OPERAND).str_arg(op.symbol()));
        }
        if op == BinOp::Add {
            if let (Some(a), Some(b)) = (x.as_str(), y.as_str()) {
                let mut s = String::with_capacity(a.len() + b.len());
                s.push_str(a);
                s.push_str(b);
                return Ok(Value::string(s));
            }
        }
        Err(err(OPERAND_TYPES).str_arg(op.symbol()).str_arg(self.describe(x)).str_arg(self.describe(y)))
    }

    fn index(&self, o: &Value, k: &Value) -> Result<Value, RuntimeError> {
        match o.tag() {
            tag::LIST => match k.as_num() {
                Some(n) => {
                    let i = go_int(n) - 1;
                    let items = o.as_list().unwrap().items.borrow();
                    if i < 0 || i as usize >= items.len() {
                        return Err(err(LIST_INDEX_RANGE));
                    }
                    Ok(items[i as usize].clone())
                }
                None => Err(err(LIST_INDEX_NUMBER)),
            },
            tag::DICT => {
                let Some(key) = Key::new(k.clone()) else {
                    return Err(err(UNSUPPORTED).str_arg("dictionary as a key"));
                };
                match o.as_dict().unwrap().map.borrow().get(&key) {
                    Some(v) => Ok(v.clone()),
                    None => Err(err(DICT_KEY).arg(k.clone())),
                }
            }
            _ => match k.as_num() {
                Some(n) => {
                    let i = go_int(n) - 1;
                    let s = o.as_str().unwrap();
                    match (i >= 0).then(|| s.chars().nth(i as usize)).flatten() {
                        Some(c) => Ok(Value::string(c.to_string())),
                        None => Err(err(STRING_INDEX_RANGE)),
                    }
                }
                None => Err(err(MEMBER_UNSUPPORTED).str_arg("string")),
            },
        }
    }

    /// A push/pop/clear may not change a constant's list.
    fn require_mutable(&self, target: u32, base: usize) -> Result<(), RuntimeError> {
        if target == NONE {
            return Ok(());
        }
        let var = &self.prog.vars[target as usize];
        for s in &var.slots {
            if !self.load(s.loc, base).is_undef() && self.meta(*s, base).1 {
                return Err(err(CONSTANT).str_arg(self.name(var.name)));
            }
        }
        Ok(())
    }

    /// After a push: the new element must fit the declared list type.
    fn check_push(&self, target: u32, base: usize, list: &Value, front: bool) -> Result<(), RuntimeError> {
        if target == NONE {
            return Ok(());
        }
        let var = &self.prog.vars[target as usize];
        let Some(slot) = self.find(var, base) else { return Ok(()) };
        let (ty, _) = self.meta(slot, base);
        if ty == 0 {
            return Ok(());
        }
        let spec = &self.prog.types[ty as usize];
        let name = self.name(var.name);
        let fits = {
            let items = list.as_list().unwrap().items.borrow();
            if let (TypeKind::List(Some(elem)), false) = (&spec.kind, items.is_empty()) {
                let e = if front { &items[0] } else { &items[items.len() - 1] };
                accepts(elem, e)
            } else {
                drop(items);
                self.accepts(spec, list)
            }
        };
        if fits {
            Ok(())
        } else {
            Err(err(VARIABLE_TYPE).str_arg(name).str_arg(&spec.text).str_arg(self.describe(list)))
        }
    }

    fn call_method(&self, o: &Value, name: &str, target: u32, base: usize, args: &[Value]) -> Result<Value, RuntimeError> {
        let l = self.lang;
        match o.tag() {
            tag::STR => builtins::string_method(o.as_str().unwrap(), name, args, l),
            tag::LIST => {
                if name != l.list_clear {
                    return Err(err(METHOD_NOT_FOUND).str_arg(name));
                }
                if !args.is_empty() {
                    return Err(err(ARG_COUNT).num_arg(0.0));
                }
                self.require_mutable(target, base)?;
                o.as_list().unwrap().items.borrow_mut().clear();
                Ok(Value::NULL)
            }
            tag::DICT => Err(err(UNSUPPORTED).str_arg("dictionary method")),
            _ => Err(err(MEMBER_UNSUPPORTED).str_arg(type_name_of(o))),
        }
    }

    /// `입력받자`'s conversion (Hana's `conv.ParseInput`).
    fn parse_input(&self, ty: u32, text: &str) -> Result<Value, RuntimeError> {
        let l = self.lang;
        let name = if ty == 0 { "" } else { self.prog.types[ty as usize].text.as_str() };
        if name.is_empty() || name == l.type_string {
            return Ok(Value::str(text));
        }
        if name == l.type_number {
            let t = text.trim();
            if builtins::plain_number(t) {
                if let Ok(n) = t.parse::<f64>() {
                    return Ok(Value::num(n));
                }
            }
            return Err(err(INPUT_NUMBER).str_arg(text));
        }
        if name == l.type_boolean {
            return match text.trim() {
                t if t == l.true_word => Ok(Value::bool(true)),
                t if t == l.false_word => Ok(Value::bool(false)),
                _ => Err(err(INPUT_BOOLEAN).str_arg(text)),
            };
        }
        Err(err(INPUT_TYPE).str_arg(name))
    }
}

/// Whether a value fits a type (Runtime spec 2.2): null fits every type.
fn accepts(kind: &TypeKind, v: &Value) -> bool {
    if v.is_null() {
        return true;
    }
    match kind {
        TypeKind::Any => true,
        TypeKind::Number => v.tag() == tag::NUM,
        TypeKind::String => v.tag() == tag::STR,
        TypeKind::Boolean => v.tag() == tag::BOOL,
        TypeKind::Null => false,
        TypeKind::List(elem) => match v.as_list() {
            Some(list) => elem.as_ref().is_none_or(|e| list.items.borrow().iter().all(|x| accepts(e, x))),
            None => false,
        },
        TypeKind::Dict(args) => match v.as_dict() {
            Some(d) => args.as_ref().is_none_or(|(k, e)| d.map.borrow().iter().all(|(key, x)| accepts(k, &key.0) && accepts(e, x))),
            None => false,
        },
        TypeKind::Class(_) => false, // classes come with objects
    }
}

/// Arithmetic and comparison of two numbers.
#[inline(always)]
fn arith(op: BinOp, x: f64, y: f64) -> Result<Value, RuntimeError> {
    Ok(match op {
        BinOp::Add => boxed(x + y),
        BinOp::Sub => boxed(x - y),
        BinOp::Mul => boxed(x * y),
        BinOp::Div => {
            if y == 0.0 {
                return Err(err(DIVIDE_BY_ZERO));
            }
            boxed(x / y)
        }
        BinOp::Mod => {
            let d = go_i64(y);
            if d == 0 {
                return Err(err(DIVIDE_BY_ZERO));
            }
            boxed(go_i64(x).wrapping_rem(d) as f64)
        }
        BinOp::Gt => Value::bool(x > y),
        BinOp::Lt => Value::bool(x < y),
        BinOp::Ge => Value::bool(x >= y),
        BinOp::Le => Value::bool(x <= y),
    })
}

/// A computed number the way Hana stores it: `num.Box` turns -0 into 0.
#[inline]
fn boxed(n: f64) -> Value {
    Value::num(if n == 0.0 { 0.0 } else { n })
}

/// Hana's `errs.TypeNameOf`: a value's type in plain English.
pub fn type_name_of(v: &Value) -> &'static str {
    match v.tag() {
        tag::NULL => "null",
        tag::NUM => "number",
        tag::STR => "string",
        tag::BOOL => "boolean",
        tag::DICT => "dict",
        _ => "object",
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_code_is_in_hanas_catalog() {
        use super::codes::*;
        for code in [
            VARIABLE_NOT_FOUND, VARIABLE_TYPE, ARGUMENT_TYPE, RETURN_TYPE, CONSTANT, OPERAND_TYPES, NULL_OPERAND,
            UNKNOWN_OPERATOR, DIVIDE_BY_ZERO, NOT_BOOLEAN, RANGE_NUMBERS, NOT_ITERABLE, LIST_INDEX_RANGE,
            LIST_INDEX_NUMBER, STRING_INDEX_RANGE, MEMBER_UNSUPPORTED, MEMBER_ON_STRING, DICT_KEY,
            STRING_INDEX_ASSIGN, NOT_A_LIST, LIST_EMPTY, TOO_MANY_ARGUMENTS, MISSING_ARGUMENT,
            FUNCTION_NOT_FOUND, NOT_CALLABLE, METHOD_NOT_FOUND, ARG_COUNT, METHOD_ARG_NUMBER,
            METHOD_ARG_STRING, CALL_TOO_DEEP, TO_NUMBER_FAILED, TO_NUMBER_INVALID, TO_CODE, TO_TEXT,
            INPUT_NUMBER, INPUT_BOOLEAN, INPUT_TYPE,
        ] {
            assert!(crate::catalog::CATALOG.iter().any(|e| e.0 == code), "{code} is not a Hana error code");
        }
    }
}
