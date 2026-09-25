//! The register machine that runs a compiled [`Program`].
//!
//! Frames live on one value stack; a call moves its arguments into the new
//! frame's first registers. Methods, constructors, getters and setters run
//! in frames that carry their object (`this`). Errors, `반복을 끝내자`
//! outside any loop of its function, and returns that must pass a
//! `마무리는 항상` travel as signals: they unwind to the nearest handler or
//! loop that takes them, leaving frames that have none (Hana lets a break in
//! a function end the caller's loop).

use std::collections::{HashMap, HashSet};
use std::io::Write;

use haru_abi::tag;

use crate::builtins;
use crate::bytecode::*;
use crate::error::RuntimeError;
use crate::format::{display, go_i64, go_int};
use crate::lang::Lang;
use crate::modules::{FnRef, Runtime};
use crate::symbol;
use crate::value::{FuncObj, Key, Value, CLASS};

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
    pub const THIS_NOT_BOUND: &str = "ReferenceError.ThisNotBound";
    pub const SUPER_OUTSIDE: &str = "ReferenceError.SuperOutsideMethod";
    pub const STATIC_OUTSIDE: &str = "ReferenceError.StaticOutsideMethod";
    pub const CLASS_NOT_FOUND: &str = "ReferenceError.ClassNotFound";
    pub const INSTANTIATE_INTERFACE: &str = "InstantiationError.Interface";
    pub const INSTANTIATE_ABSTRACT: &str = "InstantiationError.AbstractClass";
    pub const STATIC_METHOD_NOT_FOUND: &str = "MethodNotFoundError.StaticMethodNotFound";
    pub const STATIC_MEMBER_NOT_FOUND: &str = "KeyError.StaticMemberNotFound";
    pub const SUPER_MEMBER_METHOD: &str = "TypeError.SuperMemberMustBeMethod";
    pub const PRIVATE_METHOD: &str = "AccessViolationError.PrivateMethodAccess";
    pub const PRIVATE_FIELD: &str = "AccessViolationError.PrivateFieldAccess";
    pub const PROTECTED_METHOD: &str = "AccessViolationError.ProtectedMethodAccess";
    pub const PROTECTED_FIELD: &str = "AccessViolationError.ProtectedFieldAccess";
    pub const IMPORT_TARGET: &str = "ImportError.ImportTargetNotFound";
    pub const IMPORT_PACKAGE: &str = "ImportError.ImportPackageNotFound";
    pub const CLASS_CONFLICT: &str = "ImportError.ImportClassConflict";
    pub const CLASS_CONFLICT_OWN: &str = "ImportError.ImportClassConflictOwn";

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
    /// `돌려주자` passing `마무리는 항상` blocks (or at the top level).
    Return(Value),
}

impl From<RuntimeError> for Signal {
    fn from(e: RuntimeError) -> Signal {
        Signal::Error(e)
    }
}

type Flow<T> = Result<T, Signal>;

/// What the caller does with a frame's result.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Post {
    Value,
    /// Constructors and setters.
    Discard,
    /// `<기호 같다>` for `==` (or `!=`, negated).
    Equals { neg: bool },
    /// A module's top-level code: when it ends the module is loaded.
    ModuleInit(u32),
}

/// Hana's interpreter: whose classes, static variables and words code sees.
/// The program has one; each imported module gets one when it loads. Calls
/// run in the caller's; a module's top-level code runs in the module's.
struct Namespace {
    lang: &'static Lang,
    classes: HashMap<u32, u32>,
    interfaces: HashSet<u32>,
    /// Which module a class name was brought from (for conflicts).
    owners: HashMap<u32, String>,
    statics: HashMap<(u32, u32), Value>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ModState {
    Unloaded,
    Loading,
    Loaded,
}

struct Frame {
    proto: u32,
    pc: usize,
    base: usize,
    argc: u16,
    /// Call nesting when the frame started (see `Op::Enter`).
    depth: u32,
    /// Whether a call expression started it (and so counts in the nesting).
    counted: bool,
    /// The caller's register for the result.
    ret: Reg,
    post: Post,
    /// The object of a method, constructor, getter or setter (else UNDEF).
    this: Value,
    /// The class `우리` means (NONE outside methods).
    self_class: u32,
    /// The namespace the code runs in.
    ns: u32,
    /// What handlers of this frame caught, by handler index.
    pending: Vec<(u32, Signal)>,
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
    namespaces: Vec<Namespace>,
    /// The running frame's namespace (kept in step with the frame).
    ns: u32,
    module_ns: Vec<Option<u32>>,
    module_state: Vec<ModState>,
    /// What each reflection site settled on.
    reflected: HashMap<u32, u32>,
    runtime: Option<&'p Runtime>,
    length_word: u32,
    init_name: u32,
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
            namespaces: vec![Namespace::of(&prog.modules[0])],
            ns: 0,
            module_ns: {
                let mut v = vec![None; prog.modules.len()];
                v[0] = Some(0);
                v
            },
            module_state: {
                let mut v = vec![ModState::Unloaded; prog.modules.len()];
                v[0] = ModState::Loading;
                v
            },
            reflected: HashMap::new(),
            runtime: None,
            length_word: symbol::intern(prog.lang.length_word),
            init_name: symbol::intern("__init__"),
            output: Output::Stdout(Vec::new()),
            last_flush: std::time::Instant::now(),
            read_line: Box::new(|| None),
        }
    }

    /// The registry of native modules (the standard library) imports use.
    pub fn with_runtime(mut self, rt: &'p Runtime) -> Vm<'p> {
        self.runtime = Some(rt);
        self
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
        let main = &self.prog.protos[self.prog.modules[0].main as usize];
        self.stack.resize(main.nregs as usize, Value::UNDEF);
        self.frames.push(Frame {
            proto: 0,
            pc: 0,
            base: 0,
            argc: 0,
            depth: 0,
            counted: false,
            ret: 0,
            post: Post::Value,
            this: Value::UNDEF,
            self_class: NONE,
            ns: 0,
            pending: Vec::new(),
        });
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

    /// Hands a signal to the innermost handler or loop that takes it,
    /// leaving frames that have none. Fails when it reaches the top.
    fn unwind(&mut self, mut signal: Signal) -> Result<(), RuntimeError> {
        loop {
            let frame = self.frames.last_mut().unwrap();
            let proto = &self.prog.protos[frame.proto as usize];
            let at = frame.pc.saturating_sub(1) as u32;
            let takes = |k: HandlerKind| match signal {
                Signal::Error(_) => true,
                Signal::Break => k != HandlerKind::Catch,
                Signal::Return(_) => k == HandlerKind::Finally,
            };
            let handler = proto
                .handlers
                .iter()
                .enumerate()
                .filter(|(_, h)| h.start <= at && at < h.end && takes(h.kind))
                .min_by_key(|(_, h)| h.end - h.start);
            let lp = match signal {
                Signal::Break => proto.loops.iter().filter(|l| l.start <= at && at < l.end).min_by_key(|l| l.end - l.start),
                _ => None,
            };
            match (handler, lp) {
                (Some((i, h)), l) if l.is_none_or(|l| h.end - h.start <= l.end - l.start) => {
                    frame.pc = h.target as usize;
                    self.depth = frame.depth + h.open_calls;
                    let key = i as u32;
                    frame.pending.retain(|(k, _)| *k != key);
                    frame.pending.push((key, signal));
                    return Ok(());
                }
                (_, Some(l)) => {
                    frame.pc = l.exit as usize;
                    self.depth = frame.depth;
                    return Ok(());
                }
                _ => {}
            }
            // Nothing in this frame takes it.
            if let Post::ModuleInit(m) = frame.post {
                // The import fails with it; the module may be loaded again later.
                self.module_state[m as usize] = ModState::Unloaded;
                let frame = self.frames.pop().unwrap();
                self.stack.truncate(frame.base);
                continue;
            }
            if self.frames.len() == 1 {
                return Err(match signal {
                    Signal::Error(e) => e,
                    Signal::Break => err("break"),
                    Signal::Return(_) => err("return"),
                });
            }
            match signal {
                Signal::Return(v) => match self.finish_call(v, false) {
                    Ok(()) => return Ok(()),
                    Err(s) => signal = s,
                },
                _ => {
                    // Carry on from the caller's call instruction.
                    let frame = self.frames.pop().unwrap();
                    self.stack.truncate(frame.base);
                }
            }
        }
    }

    // ---- variables

    fn get(&self, loc: Loc, fi: usize) -> Value {
        let f = &self.frames[fi];
        match loc {
            Loc::Reg(r) => self.stack[f.base + r as usize].clone(),
            Loc::Global(g) => self.globals[g as usize].clone(),
            Loc::This(name) => match f.this.as_object() {
                Some(o) => o.props.borrow().get(&name).cloned().unwrap_or(Value::UNDEF),
                None => Value::UNDEF,
            },
        }
    }

    fn defined(&self, loc: Loc, fi: usize) -> bool {
        let f = &self.frames[fi];
        match loc {
            Loc::Reg(r) => !self.stack[f.base + r as usize].is_undef(),
            Loc::Global(g) => !self.globals[g as usize].is_undef(),
            Loc::This(name) => f.this.as_object().is_some_and(|o| o.props.borrow().contains_key(&name)),
        }
    }

    fn store(&mut self, loc: Loc, fi: usize, v: Value) {
        let base = self.frames[fi].base;
        match loc {
            Loc::Reg(r) => self.stack[base + r as usize] = v,
            Loc::Global(g) => self.globals[g as usize] = v,
            Loc::This(name) => {
                if let Some(o) = self.frames[fi].this.as_object() {
                    o.props.borrow_mut().insert(name, v);
                }
            }
        }
    }

    /// The first slot of the chain that holds a variable.
    fn find(&self, var: &Var, fi: usize) -> Option<Slot> {
        var.slots.iter().copied().find(|s| self.defined(s.loc, fi))
    }

    fn name(&self, id: u32) -> &'static str {
        symbol::name(id)
    }

    /// A name that is no variable: the class of that name, or an error.
    fn missing(&self, name: u32) -> Flow<Value> {
        if self.has_class(name) {
            return Ok(Value::class(name));
        }
        Err(err(VARIABLE_NOT_FOUND).str_arg(self.name(name)).into())
    }

    fn read_var(&self, var_id: u32, fi: usize) -> Flow<Value> {
        let var = &self.prog.vars[var_id as usize];
        match self.find(var, fi) {
            Some(s) => Ok(self.get(s.loc, fi)),
            None => self.missing(var.name),
        }
    }

    /// The declared type and constness of the variable in `slot`.
    fn meta(&self, slot: Slot, fi: usize) -> (u32, bool) {
        match (slot.meta, slot.loc) {
            (Some(m), _) => meta_parts(&self.get(m, fi)),
            // A property read as a variable: its field's declared type.
            (None, Loc::This(name)) => match self.frames[fi].this.as_object() {
                Some(o) => (self.class(o.class).and_then(|c| c.field_types.get(&name).copied()).unwrap_or(0), false),
                None => (0, false),
            },
            _ => (0, false),
        }
    }

    /// The class a name means in the running namespace.
    fn class(&self, sym: u32) -> Option<&'p ClassInfo> {
        let prog = self.prog;
        self.namespaces[self.ns as usize].classes.get(&sym).map(|&id| &prog.classes[id as usize])
    }

    fn has_class(&self, sym: u32) -> bool {
        self.namespaces[self.ns as usize].classes.contains_key(&sym)
    }

    /// `정하자` (Hana's `assignVariable`).
    fn declare(&mut self, var_id: u32, fi: usize, val: Value, ty: u32, konst: bool) -> Flow<()> {
        let prog = self.prog;
        let var = &prog.vars[var_id as usize];
        if ty != 0 {
            self.check_type(VARIABLE_TYPE, self.name(var.name), ty, &val)?;
        }
        match self.find(var, fi) {
            Some(slot) => {
                let (declared, constant) = self.meta(slot, fi);
                if declared != 0 && declared != ty {
                    self.check_type(VARIABLE_TYPE, self.name(var.name), declared, &val)?;
                }
                if constant {
                    return Err(err(CONSTANT).str_arg(self.name(var.name)).into());
                }
                self.store(slot.loc, fi, val);
            }
            None => {
                let slot = var.slots[0];
                self.store(slot.loc, fi, val);
                if let Some(m) = slot.meta {
                    self.store(m, fi, meta_value(ty, konst));
                }
            }
        }
        Ok(())
    }

    /// An assignment statement: only an existing variable changes.
    fn assign(&mut self, var_id: u32, fi: usize, v: Value) -> Flow<()> {
        let var = &self.prog.vars[var_id as usize];
        if let Some(slot) = self.find(var, fi) {
            let (declared, constant) = self.meta(slot, fi);
            if declared != 0 {
                self.check_type(VARIABLE_TYPE, self.name(var.name), declared, &v)?;
            }
            if constant {
                return Err(err(CONSTANT).str_arg(self.name(var.name)).into());
            }
            self.store(slot.loc, fi, v);
        }
        Ok(())
    }

    fn check_type(&self, code: &str, name: &str, ty: u32, v: &Value) -> Flow<()> {
        let spec = &self.prog.types[ty as usize];
        if self.accepts(&spec.kind, v) {
            return Ok(());
        }
        Err(err(code).str_arg(name).str_arg(&spec.text).str_arg(self.describe(v)).into())
    }

    /// Whether a value fits a type (Runtime spec 2.2): null fits every type.
    fn accepts(&self, kind: &TypeKind, v: &Value) -> bool {
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
                Some(list) => elem.as_ref().is_none_or(|e| list.items.borrow().iter().all(|x| self.accepts(e, x))),
                None => false,
            },
            TypeKind::Dict(args) => match v.as_dict() {
                Some(d) => args
                    .as_ref()
                    .is_none_or(|(k, e)| d.map.borrow().iter().all(|(key, x)| self.accepts(k, &key.0) && self.accepts(e, x))),
                None => false,
            },
            // A class, a parent class, or an interface of either.
            TypeKind::Class(name) => match v.as_object() {
                Some(o) => o.class == *name || self.class(o.class).is_some_and(|c| c.supertypes.contains(name)),
                None => false,
            },
        }
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
            tag::OBJECT => self.name(v.as_object().unwrap().class),
            _ => "?",
        }
    }

    // ---- calls

    /// Starts a function. The arguments are in `arg_base..arg_base+argc`
    /// (absolute stack positions); the caller's pc must already be saved.
    #[allow(clippy::too_many_arguments)]
    fn call_proto(
        &mut self,
        proto_id: u32,
        arg_base: usize,
        argc: u16,
        ret: Reg,
        this: Value,
        self_class: u32,
        post: Post,
        counted: bool,
    ) -> Flow<()> {
        let proto = &self.prog.protos[proto_id as usize];
        let nparams = proto.params.len();
        if proto.raw_params {
            // Only the first argument, bound as it is.
            let base = self.stack.len();
            self.stack.resize(base + proto.nregs as usize, Value::UNDEF);
            if let (Some(p), true) = (proto.params.first(), argc > 0) {
                self.stack[base + p.slot as usize] = std::mem::replace(&mut self.stack[arg_base], Value::UNDEF);
            }
            let depth = self.depth;
            let ns = self.ns;
            self.frames.push(Frame { proto: proto_id, pc: 0, base, argc, depth, counted, ret, post, this, self_class, ns, pending: Vec::new() });
            return Ok(());
        }
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
        let depth = self.depth;
        let ns = self.ns;
            self.frames.push(Frame { proto: proto_id, pc: 0, base, argc, depth, counted, ret, post, this, self_class, ns, pending: Vec::new() });
        Ok(())
    }

    fn call_plain(&mut self, proto: u32, arg_base: usize, argc: u16, ret: Reg) -> Flow<()> {
        self.call_proto(proto, arg_base, argc, ret, Value::UNDEF, NONE, Post::Value, true)
    }

    /// Calls a function value. `Ok(Some(v))` for an immediate result;
    /// `Ok(None)` when a frame was pushed.
    fn call_value(&mut self, f: &Value, arg_base: usize, argc: u16, ret: Reg, module: u32) -> Flow<Option<Value>> {
        match f.as_func() {
            Some(FuncObj::User(p)) => {
                self.call_plain(*p, arg_base, argc, ret)?;
                Ok(None)
            }
            Some(FuncObj::Builtin(b)) => {
                let args: Vec<Value> = (0..argc as usize).map(|i| self.stack[arg_base + i].clone()).collect();
                Ok(Some(builtins::call(*b, &args, self.lang)?))
            }
            Some(&FuncObj::Native { module: m, func }) => {
                let Some(rt) = self.runtime else {
                    return Err(err(UNSUPPORTED).str_arg("native module call").into());
                };
                let args: Vec<Value> = (0..argc as usize).map(|i| self.stack[arg_base + i].clone()).collect();
                Ok(Some(rt.call(FnRef { module: m, func }, &args)?))
            }
            None => match f.as_str() {
                // A name: what it means now (a built-in or function by that name).
                Some(name) => {
                    let name = name.to_string();
                    let info = &self.prog.modules[module as usize];
                    if let Some(&g) = info.globals.get(name.as_str()) {
                        let v = self.globals[g as usize].clone();
                        if matches!(v.as_func(), Some(FuncObj::Builtin(_) | FuncObj::User(_) | FuncObj::Native { .. })) {
                            return self.call_value(&v, arg_base, argc, ret, module);
                        }
                    }
                    if let Some(p) = self.function_named(module, &name) {
                        self.call_plain(p, arg_base, argc, ret)?;
                        return Ok(None);
                    }
                    Err(err(FUNCTION_NOT_FOUND).str_arg(&name).into())
                }
                None => Err(err(NOT_CALLABLE).into()),
            },
        }
    }

    /// A top-level function by name: the running module's, else the program's.
    fn function_named(&self, module: u32, name: &str) -> Option<u32> {
        let prog = self.prog;
        prog.modules[module as usize].functions.get(name).or_else(|| prog.modules[0].functions.get(name)).copied()
    }

    /// Hana's access check (`errs.AccessViolation`): a private member only
    /// from a method of that very object, a protected one from any method.
    fn check_access(&self, fi: usize, obj: &Value, access: Access, method: bool, name: u32) -> Flow<()> {
        if access == Access::Public {
            return Ok(());
        }
        let this = &self.frames[fi].this;
        let allowed = !this.is_undef() && (access == Access::Protected || this.same_object(obj));
        if allowed {
            return Ok(());
        }
        let code = match (access, method) {
            (Access::Protected, true) => PROTECTED_METHOD,
            (Access::Protected, false) => PROTECTED_FIELD,
            (_, true) => PRIVATE_METHOD,
            _ => PRIVATE_FIELD,
        };
        Err(err(code).str_arg(self.name(name)).into())
    }

    fn member_of(&self, class: u32, name: u32) -> Member {
        self.class(class).and_then(|c| c.members.get(&name).copied()).unwrap_or_default()
    }

    // ---- the loop

    fn exec(&mut self) -> Flow<()> {
        let prog = self.prog;
        'frames: loop {
            let fi = self.frames.len() - 1;
            let frame = &self.frames[fi];
            let proto = &prog.protos[frame.proto as usize];
            self.ns = frame.ns;
            self.lang = self.namespaces[frame.ns as usize].lang;
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
            // Pushes a frame and switches to it; `resume` is where this frame goes on.
            macro_rules! enter {
                ($resume:expr, $call:expr) => {{
                    self.frames[fi].pc = $resume;
                    if let Err(e) = $call {
                        self.frames[fi].pc = pc;
                        return Err(e);
                    }
                    continue 'frames;
                }};
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
                            let v = tri!(self.missing(name));
                            reg!(dst) = v;
                        } else {
                            reg!(dst) = v.clone();
                        }
                    }
                    Op::GetGlobal { dst, slot, name } => {
                        let v = &self.globals[slot as usize];
                        if v.is_undef() {
                            let v = tri!(self.missing(name));
                            reg!(dst) = v;
                        } else {
                            reg!(dst) = v.clone();
                        }
                    }
                    Op::GetVar { dst, var } => {
                        let v = tri!(self.read_var(var, fi));
                        reg!(dst) = v;
                    }
                    Op::Decl { var, src, ty, konst } => {
                        let v = std::mem::replace(&mut reg!(src), Value::UNDEF);
                        tri!(self.declare(var, fi, v, ty, konst));
                    }
                    Op::SetReg { slot, src } => reg!(slot) = std::mem::replace(&mut reg!(src), Value::UNDEF),
                    Op::SetGlobal { slot, src } => {
                        self.globals[slot as usize] = std::mem::replace(&mut reg!(src), Value::UNDEF);
                    }
                    Op::Assign { var, src } => {
                        let v = std::mem::replace(&mut reg!(src), Value::UNDEF);
                        tri!(self.assign(var, fi, v));
                    }
                    Op::Update { var, a, b, op } => {
                        if op == BinOp::Add && reg!(a).tag() == tag::STR && reg!(b).tag() == tag::STR {
                            let v = &prog.vars[var as usize];
                            if let Some(slot @ Slot { loc: Loc::Reg(_) | Loc::Global(_), .. }) = self.find(v, fi) {
                                if self.get(slot.loc, fi).same_object(&reg!(a)) {
                                    // The result is a string: check it as the value read.
                                    let (declared, constant) = self.meta(slot, fi);
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
                                        Loc::This(_) => unreachable!(),
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
                        tri!(self.assign(var, fi, result));
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
                            (Some(x), Some(y)) => tri!(arith(op, x, y)),
                            _ => tri!(self.slow_binary(op, x, y)),
                        };
                        reg!(dst) = v;
                    }
                    Op::Eq { dst, a, b, neg } => {
                        // An object whose class has `<기호 같다>` decides itself.
                        if let Some(o) = reg!(a).as_object() {
                            if let Some(eq) = self.class(o.class).and_then(|c| c.equals) {
                                let class = o.class;
                                let this = reg!(a).clone();
                                enter!(pc, self.call_proto(eq, base + b as usize, 1, dst, this, class, Post::Equals { neg }, false));
                            }
                        }
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
                            tri!(self.declare(var, fi, v, 0, false));
                        }
                    }

                    Op::Enter => {
                        self.depth += 1;
                        if self.depth > MAX_CALL_DEPTH {
                            self.depth -= 1;
                            fail!(err(CALL_TOO_DEEP).num_arg(MAX_CALL_DEPTH as f64));
                        }
                    }
                    Op::Leave => self.depth -= 1,
                    Op::Call { dst, proto, base: b, argc } => {
                        enter!(pc, self.call_plain(proto, base + b as usize, argc, dst));
                    }
                    Op::CallName { dst, name, var, base: b, argc } => {
                        let f = if var == NONE {
                            None
                        } else {
                            self.find(&prog.vars[var as usize], fi).map(|s| self.get(s.loc, fi))
                        };
                        match f {
                            Some(f) if matches!(f.as_func(), Some(FuncObj::Builtin(_) | FuncObj::User(_) | FuncObj::Native { .. })) => {
                                self.frames[fi].pc = pc;
                                match tri!(self.call_value(&f, base + b as usize, argc, dst, proto.module)) {
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
                        match tri!(self.call_value(&f, base + b as usize, argc, dst, proto.module)) {
                            Some(v) => {
                                reg!(dst) = v;
                                self.depth -= 1;
                            }
                            None => continue 'frames,
                        }
                    }
                    Op::MethodPrep { obj, name } => {
                        let o = reg!(obj).clone();
                        match o.tag() {
                            tag::OBJECT => {
                                let m = self.member_of(o.as_object().unwrap().class, name);
                                tri!(self.check_access(fi, &o, m.method_access, true, name));
                            }
                            CLASS => {
                                let class = o.as_class().unwrap();
                                if !self.class(class).is_some_and(|c| c.statics.contains_key(&name)) {
                                    fail!(err(STATIC_MEMBER_NOT_FOUND).str_arg(self.name(name)));
                                }
                            }
                            tag::STR | tag::LIST => {}
                            tag::DICT => fail!(err(UNSUPPORTED).str_arg("dictionary method")),
                            _ => fail!(err(MEMBER_UNSUPPORTED).str_arg(type_name_of(&o))),
                        }
                    }
                    Op::CallMethod { dst, obj, name, target, base: b, argc } => {
                        let o = reg!(obj).clone();
                        match o.tag() {
                            tag::OBJECT => {
                                let class = o.as_object().unwrap().class;
                                let proto = if name == self.init_name {
                                    self.class(class).and_then(|c| c.ctor)
                                } else {
                                    self.member_of(class, name).method
                                };
                                match proto {
                                    Some(p) => enter!(
                                        pc,
                                        self.call_proto(p, base + b as usize, argc, dst, o, class, Post::Value, true)
                                    ),
                                    None => fail!(err(METHOD_NOT_FOUND).str_arg(self.name(name))),
                                }
                            }
                            CLASS => {
                                let class = o.as_class().unwrap();
                                match self.class(class).and_then(|c| c.statics.get(&name).copied()) {
                                    Some(p) => enter!(
                                        pc,
                                        self.call_proto(p, base + b as usize, argc, dst, Value::UNDEF, class, Post::Value, true)
                                    ),
                                    None => fail!(err(STATIC_METHOD_NOT_FOUND).str_arg(self.name(name))),
                                }
                            }
                            _ => {
                                let args: Vec<Value> = (0..argc as usize).map(|i| reg!(b as usize + i).clone()).collect();
                                let v = tri!(self.call_method(&o, self.name(name), target, fi, &args));
                                reg!(dst) = v;
                                self.depth -= 1;
                            }
                        }
                    }
                    Op::SuperPrep { method } => {
                        if self.frames[fi].this.is_undef() {
                            fail!(err(SUPER_OUTSIDE));
                        }
                        if !method {
                            fail!(err(SUPER_MEMBER_METHOD));
                        }
                    }
                    Op::CallSuper { dst, name, base: b, argc } => {
                        let this = self.frames[fi].this.clone();
                        let class = this.as_object().unwrap().class;
                        // Hana starts at the parent of the object's own class.
                        let start = match self.class(class).map(|c| c.super_start) {
                            Some(Some(parent)) => parent,
                            _ => Some(class),
                        };
                        let proto = start.and_then(|s| {
                            if name == self.init_name {
                                self.class(s).and_then(|c| c.ctor)
                            } else {
                                self.member_of(s, name).method
                            }
                        });
                        match proto {
                            Some(p) => {
                                enter!(pc, self.call_proto(p, base + b as usize, argc, dst, this, class, Post::Value, true))
                            }
                            None => fail!(err(METHOD_NOT_FOUND).str_arg(self.name(name))),
                        }
                    }
                    Op::Return { src } => {
                        let v = std::mem::replace(&mut reg!(src), Value::UNDEF);
                        self.finish_call(v, false)?;
                        continue 'frames;
                    }
                    Op::ReturnNull => {
                        if fi == 0 {
                            return Ok(());
                        }
                        self.finish_call(Value::NULL, true)?;
                        continue 'frames;
                    }
                    Op::ReturnSignal { src } => {
                        let v = std::mem::replace(&mut reg!(src), Value::UNDEF);
                        fail!(Signal::Return(v));
                    }
                    Op::Break => fail!(Signal::Break),

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
                        let o = reg!(obj).clone();
                        let length = name == self.length_word;
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
                            tag::OBJECT => {
                                let class = o.as_object().unwrap().class;
                                let m = self.member_of(class, name);
                                tri!(self.check_access(fi, &o, m.field_access, false, name));
                                if let Some(g) = m.getter {
                                    enter!(skip as usize, self.call_proto(g, 0, 0, dst, o, class, Post::Value, false));
                                }
                                let v = o.as_object().unwrap().props.borrow().get(&name).cloned().unwrap_or(Value::NULL);
                                reg!(dst) = v;
                                pc = skip as usize;
                            }
                            CLASS => {
                                let class = o.as_class().unwrap();
                                match self.namespaces[self.ns as usize].statics.get(&(class, name)) {
                                    Some(v) => reg!(dst) = v.clone(),
                                    None => fail!(err(STATIC_MEMBER_NOT_FOUND).str_arg(self.name(name))),
                                }
                                pc = skip as usize;
                            }
                            _ => fail!(err(MEMBER_UNSUPPORTED).str_arg(type_name_of(&o))),
                        }
                    }
                    Op::Index { dst, obj, key } => {
                        let v = tri!(self.index(&reg!(obj), &reg!(key)));
                        reg!(dst) = v;
                    }
                    Op::IndexFail { obj, key } => {
                        let pending = self.take_pending(fi, key);
                        match reg!(obj).tag() {
                            tag::LIST => fail!(err(LIST_INDEX_NUMBER)),
                            tag::STR => fail!(err(MEMBER_ON_STRING)),
                            _ => fail!(pending),
                        }
                    }
                    Op::SetMember { obj, val, name, skip } => {
                        let o = reg!(obj).clone();
                        match o.tag() {
                            tag::STR => fail!(err(STRING_INDEX_ASSIGN)),
                            tag::LIST | tag::DICT => {}
                            tag::OBJECT => {
                                pc = skip as usize;
                                if name != NONE {
                                    let class = o.as_object().unwrap().class;
                                    let m = self.member_of(class, name);
                                    let v = reg!(val).clone();
                                    if let Some(s) = m.setter {
                                        // The setter gets the value as its only argument.
                                        reg!(val) = v;
                                        enter!(pc, self.call_proto(s, base + val as usize, 1, val, o, class, Post::Discard, false));
                                    }
                                    if let Some(&ty) = self.class(class).and_then(|c| c.field_types.get(&name)) {
                                        if ty != 0 {
                                            tri!(self.check_type(VARIABLE_TYPE, self.name(name), ty, &v));
                                        }
                                    }
                                    o.as_object().unwrap().props.borrow_mut().insert(name, v);
                                }
                            }
                            CLASS => {
                                if name != NONE {
                                    self.namespaces[self.ns as usize].statics.insert((o.as_class().unwrap(), name), reg!(val).clone());
                                }
                                pc = skip as usize;
                            }
                            _ => pc = skip as usize,
                        }
                    }
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
                    Op::SetIndexFail { obj, val, name, key, skip } => {
                        self.take_pending(fi, key);
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
                        tri!(self.require_mutable(target, fi));
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
                        if let Err(e) = self.check_push(target, fi, &l, front) {
                            if front {
                                items.borrow_mut().remove(0);
                            } else {
                                items.borrow_mut().pop();
                            }
                            fail!(e);
                        }
                    }
                    Op::CheckFieldPush { list, obj, name, front } => {
                        let o = reg!(obj).clone();
                        let Some(object) = o.as_object() else { continue };
                        let ty = self.class(object.class).and_then(|c| c.field_types.get(&name).copied()).unwrap_or(0);
                        if ty == 0 {
                            continue;
                        }
                        let l = reg!(list).clone();
                        if let Err(e) = self.check_appended(ty, self.name(name), &l, front) {
                            let items = &l.as_list().unwrap().items;
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
                        let v = if let Some(p) = self.function_named(proto.module, n) {
                            Value::func(FuncObj::User(p))
                        } else {
                            match self.find(&prog.vars[var as usize], fi).map(|s| self.get(s.loc, fi)) {
                                Some(v) if matches!(v.as_func(), Some(FuncObj::User(_))) => v,
                                _ => Value::str(n),
                            }
                        };
                        reg!(dst) = v;
                    }
                    Op::Unsupported { k } => fail!(err(UNSUPPORTED).arg(prog.consts[k as usize].clone())),
                    Op::Fail { k, args } => {
                        let code = prog.consts[k as usize].as_str().unwrap().to_string();
                        let mut e = err(&code);
                        for a in args {
                            e = e.arg(prog.consts[a as usize].clone());
                        }
                        fail!(e);
                    }

                    // ---- classes
                    Op::NewObj { dst, class } => match self.class(class) {
                        Some(c) if c.is_abstract => fail!(err(INSTANTIATE_ABSTRACT).str_arg(self.name(class))),
                        Some(_) => reg!(dst) = Value::object(class),
                        None if self.namespaces[self.ns as usize].interfaces.contains(&class) => {
                            fail!(err(INSTANTIATE_INTERFACE).str_arg(self.name(class)))
                        }
                        None => fail!(err(CLASS_NOT_FOUND).str_arg(self.name(class))),
                    },
                    Op::InitField { obj, name, src, ty } => {
                        let v = std::mem::replace(&mut reg!(src), Value::UNDEF);
                        if ty != 0 {
                            tri!(self.check_type(VARIABLE_TYPE, self.name(name), ty, &v));
                        }
                        reg!(obj).as_object().unwrap().props.borrow_mut().insert(name, v);
                    }
                    Op::CallCtor { obj, proto, class, base: b, argc } => {
                        let o = reg!(obj).clone();
                        enter!(pc, self.call_proto(proto, base + b as usize, argc, obj, o, class, Post::Discard, true));
                    }
                    Op::GetThis { dst } => {
                        let this = self.frames[fi].this.clone();
                        if this.is_undef() {
                            fail!(err(THIS_NOT_BOUND));
                        }
                        reg!(dst) = this;
                    }
                    Op::SelfOr { dst, var } => {
                        let this = self.frames[fi].this.clone();
                        reg!(dst) = if this.is_undef() { tri!(self.read_var(var, fi)) } else { this };
                    }
                    Op::GetStatic { dst } => {
                        let class = self.frames[fi].self_class;
                        if class == NONE {
                            fail!(err(STATIC_OUTSIDE));
                        }
                        reg!(dst) = Value::class(class);
                    }
                    Op::StaticOr { dst, var } => {
                        let class = self.frames[fi].self_class;
                        reg!(dst) = if class == NONE { tri!(self.read_var(var, fi)) } else { Value::class(class) };
                    }
                    Op::TypeValue { dst, var, name } => {
                        let v = match self.find(&prog.vars[var as usize], fi) {
                            Some(s) => self.get(s.loc, fi),
                            None if self.has_class(name) => Value::class(name),
                            None => Value::str(self.name(name)),
                        };
                        reg!(dst) = v;
                    }
                    Op::InstanceOf { dst, a, b } => {
                        let is = match (reg!(a).as_object(), reg!(b).as_class()) {
                            (Some(o), Some(target)) => {
                                o.class == target || self.class(o.class).is_some_and(|c| c.lineage.contains(&target))
                            }
                            _ => false,
                        };
                        reg!(dst) = Value::bool(is);
                    }
                    Op::SetStatic { class, name, src } => {
                        let class = if class == NONE { self.frames[fi].self_class } else { class };
                        if class != NONE {
                            let v = std::mem::replace(&mut reg!(src), Value::UNDEF);
                            self.namespaces[self.ns as usize].statics.insert((class, name), v);
                        }
                    }

                    // ---- modules
                    Op::Import { import } => {
                        let info = &prog.imports[import as usize];
                        match &info.kind {
                            ImportKind::Fail { code, args } => {
                                let mut e = err(code);
                                for a in args {
                                    e = e.str_arg(a);
                                }
                                fail!(e);
                            }
                            &ImportKind::File(m) => {
                                if self.module_state[m as usize] == ModState::Unloaded {
                                    // Run the module's top level, then come back here to bind.
                                    let ns = self.load_namespace(m);
                                    enter!(pc - 1, self.push_module(m, ns));
                                }
                                tri!(self.bind_file(info, m, fi));
                            }
                            ImportKind::Std(name) => tri!(self.bind_std(info, name, fi)),
                        }
                    }
                    Op::InitFieldsDyn { obj } => {
                        let o = reg!(obj).clone();
                        if let Some(object) = o.as_object() {
                            if let Some(c) = self.class(object.class) {
                                let class = object.class;
                                enter!(pc, self.call_proto(c.init, 0, 0, obj, o, class, Post::Discard, false));
                            }
                        }
                    }
                    Op::CallCtorDyn { obj, base: b, argc } => {
                        let o = reg!(obj).clone();
                        let class = o.as_object().map_or(NONE, |x| x.class);
                        match self.class(class).and_then(|c| c.ctor) {
                            Some(p) => enter!(pc, self.call_proto(p, base + b as usize, argc, obj, o, class, Post::Discard, true)),
                            None => self.depth -= 1,
                        }
                    }
                    Op::Reflect { dst, site, var, quoted } => {
                        let resolve = site & (1 << 31) == 0;
                        let name = match self.reflected.get(&site) {
                            Some(&n) => n,
                            // Hana rewrites the call site once the variable holds a text.
                            None => match self.find(&prog.vars[var as usize], fi).map(|s| self.get(s.loc, fi)) {
                                Some(v) if v.as_str().is_some() => {
                                    let n = symbol::intern(v.as_str().unwrap());
                                    self.reflected.insert(site, n);
                                    n
                                }
                                _ => quoted,
                            },
                        };
                        let text = self.name(name);
                        reg!(dst) = if !resolve {
                            Value::str(text)
                        } else if let Some(p) = self.function_named(proto.module, text) {
                            Value::func(FuncObj::User(p))
                        } else {
                            Value::str(text)
                        };
                    }
                    Op::MethodPrepDyn { obj, name } => {
                        let n = symbol::intern(reg!(name).as_str().unwrap_or(""));
                        let o = reg!(obj).clone();
                        match o.tag() {
                            tag::OBJECT => {
                                let m = self.member_of(o.as_object().unwrap().class, n);
                                tri!(self.check_access(fi, &o, m.method_access, true, n));
                            }
                            CLASS => {
                                let class = o.as_class().unwrap();
                                if !self.class(class).is_some_and(|c| c.statics.contains_key(&n)) {
                                    fail!(err(STATIC_MEMBER_NOT_FOUND).str_arg(self.name(n)));
                                }
                            }
                            tag::STR | tag::LIST => {}
                            tag::DICT => fail!(err(UNSUPPORTED).str_arg("dictionary method")),
                            _ => fail!(err(MEMBER_UNSUPPORTED).str_arg(type_name_of(&o))),
                        }
                    }
                    Op::CallMethodDyn { dst, obj, name, base: b, argc } => {
                        let n = symbol::intern(reg!(name).as_str().unwrap_or(""));
                        let o = reg!(obj).clone();
                        match o.tag() {
                            tag::OBJECT => {
                                let class = o.as_object().unwrap().class;
                                let p = if n == self.init_name { self.class(class).and_then(|c| c.ctor) } else { self.member_of(class, n).method };
                                match p {
                                    Some(p) => enter!(pc, self.call_proto(p, base + b as usize, argc, dst, o, class, Post::Value, true)),
                                    None => fail!(err(METHOD_NOT_FOUND).str_arg(self.name(n))),
                                }
                            }
                            CLASS => {
                                let class = o.as_class().unwrap();
                                match self.class(class).and_then(|c| c.statics.get(&n).copied()) {
                                    Some(p) => enter!(
                                        pc,
                                        self.call_proto(p, base + b as usize, argc, dst, Value::UNDEF, class, Post::Value, true)
                                    ),
                                    None => fail!(err(STATIC_METHOD_NOT_FOUND).str_arg(self.name(n))),
                                }
                            }
                            _ => {
                                let args: Vec<Value> = (0..argc as usize).map(|i| reg!(b as usize + i).clone()).collect();
                                let v = tri!(self.call_method(&o, self.name(n), NONE, fi, &args));
                                reg!(dst) = v;
                                self.depth -= 1;
                            }
                        }
                    }

                    // ---- errors
                    Op::Throw { src } => {
                        let v = std::mem::replace(&mut reg!(src), Value::UNDEF);
                        fail!(RuntimeError::thrown(v));
                    }
                    Op::CatchIs { key, class, to } => {
                        let fits = self.frames[fi].pending.iter().any(|(k, s)| {
                            *k == key
                                && matches!(s, Signal::Error(e) if e.thrown_value().and_then(|v| v.as_object()).is_some_and(|o| {
                                    o.class == class || self.class(o.class).is_some_and(|c| c.lineage.contains(&class))
                                }))
                        });
                        if fits {
                            pc = to as usize;
                        }
                    }
                    Op::CatchBind { key, slot } => {
                        let caught = self.take_pending(fi, key);
                        reg!(slot) = self.error_object(caught);
                    }
                    Op::Rethrow { key } => {
                        let signal = self.take_pending(fi, key);
                        fail!(signal);
                    }
                }
            }
        }
    }

    /// A module's namespace, made fresh when it loads (Hana starts a new
    /// sub-interpreter each time a failed module is imported again).
    fn load_namespace(&mut self, m: u32) -> u32 {
        let prog = self.prog;
        let info = &prog.modules[m as usize];
        let (a, b) = info.global_range;
        for g in a..b {
            self.globals[g as usize] = prog.globals[g as usize].clone();
        }
        let ns = self.namespaces.len() as u32;
        self.namespaces.push(Namespace::of(info));
        self.module_ns[m as usize] = Some(ns);
        ns
    }

    /// Starts a module's top-level code (the caller's pc is already saved).
    fn push_module(&mut self, m: u32, ns: u32) -> Flow<()> {
        self.module_state[m as usize] = ModState::Loading;
        let proto = self.prog.modules[m as usize].main;
        let base = self.stack.len();
        self.stack.resize(base + self.prog.protos[proto as usize].nregs as usize, Value::UNDEF);
        let depth = self.depth;
        self.frames.push(Frame {
            proto,
            pc: 0,
            base,
            argc: 0,
            depth,
            counted: false,
            ret: 0,
            post: Post::ModuleInit(m),
            this: Value::UNDEF,
            self_class: NONE,
            ns,
            pending: Vec::new(),
        });
        Ok(())
    }

    /// Hana's `bringModuleTypes`: the module's classes and interfaces become
    /// known here under their own names; a name here that came from elsewhere
    /// is a conflict unless the statement renames that class.
    fn bring_types(&mut self, info: &ImportInfo, module_ns: u32) -> Result<(), RuntimeError> {
        let here = self.ns as usize;
        let there = module_ns as usize;
        let skip = [symbol::intern(self.namespaces[there].lang.error_class), symbol::intern(self.namespaces[here].lang.error_class)];
        let mut classes: Vec<(u32, u32)> = self.namespaces[there].classes.iter().map(|(n, id)| (*n, *id)).collect();
        classes.sort_by_key(|(n, _)| self.name(*n));
        for (name, id) in classes {
            if skip.contains(&name) {
                continue;
            }
            let mine = self.namespaces[there].owners.get(&name).cloned().unwrap_or_else(|| info.source.clone());
            if self.namespaces[here].classes.contains_key(&name) {
                let theirs = self.namespaces[here].owners.get(&name).cloned().unwrap_or_default();
                if theirs != mine && !info.aliased.contains(&name) {
                    return Err(conflict(&info.source, self.name(name), &theirs));
                }
                continue;
            }
            self.namespaces[here].classes.insert(name, id);
            self.namespaces[here].owners.insert(name, mine);
        }
        let mut ifaces: Vec<u32> = self.namespaces[there].interfaces.iter().copied().collect();
        ifaces.sort_by_key(|n| self.name(*n));
        for name in ifaces {
            let mine = self.namespaces[there].owners.get(&name).cloned().unwrap_or_else(|| info.source.clone());
            if self.namespaces[here].interfaces.contains(&name) {
                let theirs = self.namespaces[here].owners.get(&name).cloned().unwrap_or_default();
                if theirs != mine && !info.aliased.contains(&name) {
                    return Err(conflict(&info.source, self.name(name), &theirs));
                }
                continue;
            }
            self.namespaces[here].interfaces.insert(name);
            self.namespaces[here].owners.insert(name, mine);
        }
        Ok(())
    }

    /// Binds what an import of a file module asks for (Hana's `bindImports`).
    fn bind_file(&mut self, info: &ImportInfo, m: u32, fi: usize) -> Result<(), RuntimeError> {
        let prog = self.prog;
        let module = &prog.modules[m as usize];
        let there = self.module_ns[m as usize].unwrap();
        self.bring_types(info, there)?;
        let here = self.ns as usize;
        if info.all {
            for (name, proto) in &module.all_functions {
                self.store(info.all_slots[name], fi, Value::func(FuncObj::User(*proto)));
            }
            for (name, id) in self.namespaces[there as usize].classes.clone() {
                if module.classes.contains_key(&name) {
                    self.namespaces[here].classes.insert(name, id);
                }
            }
            for name in module.interfaces.clone() {
                self.namespaces[here].interfaces.insert(name);
            }
        }
        for (target, bind, slot) in &info.items {
            let target_sym = symbol::intern(target);
            let bind_sym = symbol::intern(bind);
            // A variable of the module (a built-in, an import of its own, ...).
            if let Some(&g) = module.globals.get(target) {
                let v = self.globals[g as usize].clone();
                if !v.is_undef() {
                    self.store(*slot, fi, v);
                    continue;
                }
            }
            // A class: known here under the name the statement gives it.
            if let Some(&id) = self.namespaces[there as usize].classes.get(&target_sym) {
                let mine = self.namespaces[there as usize].owners.get(&target_sym).cloned().unwrap_or_else(|| info.source.clone());
                if self.namespaces[here].classes.contains_key(&bind_sym) {
                    let theirs = self.namespaces[here].owners.get(&bind_sym).cloned().unwrap_or_default();
                    if theirs != mine {
                        return Err(conflict(&info.source, bind, &theirs));
                    }
                }
                self.namespaces[here].classes.insert(bind_sym, id);
                self.namespaces[here].owners.insert(bind_sym, mine);
                continue;
            }
            if self.namespaces[there as usize].interfaces.contains(&target_sym) {
                self.namespaces[here].interfaces.insert(bind_sym);
                continue;
            }
            if let Some(&p) = module.functions.get(target) {
                self.store(*slot, fi, Value::func(FuncObj::User(p)));
                continue;
            }
            return Err(err(IMPORT_TARGET).str_arg(&info.source).str_arg(target));
        }
        Ok(())
    }

    /// Binds functions of a standard module (Hana's core native modules).
    fn bind_std(&mut self, info: &ImportInfo, name: &str, fi: usize) -> Result<(), RuntimeError> {
        let lang = self.lang.name;
        let Some(rt) = self.runtime else {
            return Err(err(IMPORT_PACKAGE).str_arg(name));
        };
        let Some(m) = rt.module(lang, name) else {
            return Err(err(IMPORT_PACKAGE).str_arg(name));
        };
        if info.all {
            for (fname, slot) in &info.all_slots {
                if let Some(f) = rt.function(m, lang, fname) {
                    self.store(*slot, fi, rt.function_value(f));
                }
            }
        }
        for (target, _, slot) in &info.items {
            match rt.function(m, lang, target) {
                Some(f) => self.store(*slot, fi, rt.function_value(f)),
                None => return Err(err(IMPORT_TARGET).str_arg(name).str_arg(target)),
            }
        }
        Ok(())
    }

    fn take_pending(&mut self, fi: usize, key: u32) -> Signal {
        let pending = &mut self.frames[fi].pending;
        match pending.iter().position(|(k, _)| *k == key) {
            Some(i) => pending.remove(i).1,
            None => Signal::Error(err(UNSUPPORTED).str_arg("lost signal")),
        }
    }

    /// What a `오류가 발생했다면` handler receives: a thrown object as it is,
    /// anything else as an `[오류]` whose `메시지` is the error's text.
    fn error_object(&self, caught: Signal) -> Value {
        let e = match caught {
            Signal::Error(e) => e,
            _ => err("unexpected"),
        };
        if let Some(v) = e.thrown_value() {
            if v.as_object().is_some() {
                return v.clone();
            }
        }
        let obj = Value::object(symbol::intern(self.lang.error_class));
        let message = e.localize(None, self.lang);
        obj.as_object().unwrap().props.borrow_mut().insert(symbol::intern(self.lang.error_message), Value::string(message));
        obj
    }

    /// Returns from the running function with `v` (`fell_off`: its body
    /// ended without `돌려주자`). The declared return type is checked in the
    /// caller, as Hana checks it after the body.
    fn finish_call(&mut self, v: Value, fell_off: bool) -> Flow<()> {
        let frame = self.frames.pop().unwrap();
        self.stack.truncate(frame.base);
        if frame.counted {
            self.depth -= 1;
        }
        let proto = &self.prog.protos[frame.proto as usize];
        let result = match frame.post {
            Post::Discard => return Ok(()),
            Post::ModuleInit(m) => {
                self.module_state[m as usize] = ModState::Loaded;
                return Ok(());
            }
            Post::Value => {
                if proto.return_type != 0 {
                    self.check_type(RETURN_TYPE, &proto.name, proto.return_type, &v)?;
                }
                v
            }
            // Hana: no return is false for `==` and true for `!=`.
            Post::Equals { neg } if fell_off => Value::bool(neg),
            Post::Equals { neg: true } => match v.as_bool() {
                Some(b) => Value::bool(!b),
                None => v,
            },
            Post::Equals { neg: false } => v,
        };
        let caller = self.frames.last().unwrap();
        self.stack[caller.base + frame.ret as usize] = result;
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
    fn require_mutable(&self, target: u32, fi: usize) -> Result<(), RuntimeError> {
        if target == NONE {
            return Ok(());
        }
        let var = &self.prog.vars[target as usize];
        for s in &var.slots {
            if self.defined(s.loc, fi) && self.meta(*s, fi).1 {
                return Err(err(CONSTANT).str_arg(self.name(var.name)));
            }
        }
        Ok(())
    }

    /// After a push: the new element must fit the declared list type.
    fn check_push(&self, target: u32, fi: usize, list: &Value, front: bool) -> Result<(), RuntimeError> {
        if target == NONE {
            return Ok(());
        }
        let var = &self.prog.vars[target as usize];
        let Some(slot) = self.find(var, fi) else { return Ok(()) };
        let (ty, _) = self.meta(slot, fi);
        if ty == 0 {
            return Ok(());
        }
        self.check_appended(ty, self.name(var.name), list, front)
    }

    /// Hana's `CheckAppended`: only the element just put on the list needs a
    /// test when the type is a list of something; otherwise the whole value.
    fn check_appended(&self, ty: u32, name: &str, list: &Value, front: bool) -> Result<(), RuntimeError> {
        let spec = &self.prog.types[ty as usize];
        let fits = {
            let items = list.as_list().unwrap().items.borrow();
            if let (TypeKind::List(Some(elem)), false) = (&spec.kind, items.is_empty()) {
                let e = if front { &items[0] } else { &items[items.len() - 1] };
                self.accepts(elem, e)
            } else {
                drop(items);
                self.accepts(&spec.kind, list)
            }
        };
        if fits {
            Ok(())
        } else {
            Err(err(VARIABLE_TYPE).str_arg(name).str_arg(&spec.text).str_arg(self.describe(list)))
        }
    }

    fn call_method(&self, o: &Value, name: &str, target: u32, fi: usize, args: &[Value]) -> Result<Value, RuntimeError> {
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
                self.require_mutable(target, fi)?;
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

impl Namespace {
    fn of(m: &ModuleInfo) -> Namespace {
        Namespace {
            lang: m.lang,
            classes: m.classes.clone(),
            interfaces: m.interfaces.clone(),
            owners: HashMap::new(),
            statics: HashMap::new(),
        }
    }
}

/// Hana's `ImportClassConflict` / `...Own` (the program's own class).
fn conflict(source: &str, class: &str, theirs: &str) -> RuntimeError {
    if theirs.is_empty() {
        err(CLASS_CONFLICT_OWN).str_arg(source).str_arg(class)
    } else {
        err(CLASS_CONFLICT).str_arg(source).str_arg(class).str_arg(theirs)
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
            INPUT_NUMBER, INPUT_BOOLEAN, INPUT_TYPE, THIS_NOT_BOUND, SUPER_OUTSIDE, STATIC_OUTSIDE,
            CLASS_NOT_FOUND, INSTANTIATE_INTERFACE, INSTANTIATE_ABSTRACT, STATIC_METHOD_NOT_FOUND,
            STATIC_MEMBER_NOT_FOUND, SUPER_MEMBER_METHOD, PRIVATE_METHOD, PRIVATE_FIELD, PROTECTED_METHOD,
            PROTECTED_FIELD, IMPORT_TARGET, IMPORT_PACKAGE, CLASS_CONFLICT, CLASS_CONFLICT_OWN,
        ] {
            assert!(crate::catalog::CATALOG.iter().any(|e| e.0 == code), "{code} is not a Hana error code");
        }
    }
}
