//! The JIT: a function's bytecode as native code (Cranelift), compiled the
//! first time the function starts.
//!
//! The native code keeps the interpreter's frame and registers: values stay
//! in the frame's registers on the VM's stack, so the interpreter and the
//! native code can take turns on one frame. An instruction gets a fast path
//! for the common case (numbers, moves, jumps); anything else — and any fast
//! path whose case does not hold — runs through the interpreter, which does
//! that one instruction (`exec_mode::<true>`) and says where to go on. So the
//! native code never decides anything the interpreter would decide
//! differently: the language is the interpreter's.
//!
//! A call starts the callee's frame as the interpreter does and runs it to
//! its end right there (natively when it has code). An error goes to the
//! frame's own handlers first (`catch_here`); what none takes leaves the
//! function, and the code that started it unwinds on as usual.

use std::mem;

use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{types, AbiParam, BlockArg, InstBuilder, JumpTableData, MemFlags, Signature, TrapCode};
use cranelift_codegen::ir::{Block, SigRef, StackSlotData, StackSlotKind, Value as V};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::Module;

use super::*;
use crate::value::ObjObj;

mod loops;

/// What the helpers answer instead of a pc.
const RETURNED: u32 = 0xFFFF_FFF0;
const ERR: u32 = 0xFFFF_FFF1;

/// What compiled code returns: the frame returned (it is gone and its result
/// is in the caller's register), it failed (the signal is in `jit_signal`),
/// or the program's top level ended.
const S_RETURNED: u32 = 0;
const S_ERR: u32 = 1;
const S_END: u32 = 2;

/// Where compiled code finds its registers. The helpers update it whenever
/// the stack may have moved.
#[repr(C)]
pub(super) struct Env {
    regs: *mut Value,
    globals: *mut Value,
    vm: *mut Vm<'static>,
    fi: usize,
    /// How many arguments the call passed.
    argc: usize,
    /// The frame's first register on the stack.
    base: usize,
    /// Where the code starts: 0, or the pc of a loop the interpreter was
    /// running when the function became hot.
    start: usize,
}

/// `Env`'s size, for the callee's one in a caller's native frame.
const ENV_SIZE: u32 = mem::size_of::<Env>() as u32;

impl Env {
    unsafe fn refresh(&mut self) {
        let vm = &mut *self.vm;
        if let Some(f) = vm.frames.get(self.fi) {
            self.regs = vm.stack.as_mut_ptr().add(f.base);
        }
        self.globals = vm.globals.as_mut_ptr();
    }
}

pub(super) type Code = unsafe extern "C" fn(*mut Env) -> u32;

pub(super) enum Done {
    Returned,
    End,
    Failed(Signal),
}

/// Runs the compiled code of frame `fi` (the running frame) from `start`.
pub(super) fn invoke(vm: &mut Vm, code: Code, fi: usize, start: usize) -> Done {
    let base = vm.frames[fi].base;
    let mut env = Env {
        regs: unsafe { vm.stack.as_mut_ptr().add(base) },
        globals: vm.globals.as_mut_ptr(),
        vm: vm as *mut Vm as *mut Vm<'static>,
        fi,
        argc: vm.frames[fi].argc as usize,
        base,
        start,
    };
    match unsafe { code(&mut env) } {
        S_RETURNED => Done::Returned,
        S_END => Done::End,
        _ => Done::Failed(vm.jit_signal.take().expect("compiled code failed without a signal")),
    }
}

enum State {
    Untried,
    Failed,
    Ready(Code),
}

/// Where an `Rc`'s strong count is, before the value its pointer points at
/// (std's `RcInner` is `repr(C)`: strong, weak, value). Compiled code counts
/// references itself when `rc_layout_holds` finds it so.
const RC_STRONG: i32 = -16;

/// Whether the strong count of every kind of heap value is at `RC_STRONG`.
fn rc_layout_holds() -> bool {
    use crate::value::{DictObj, ListObj, ObjObj, ResObj, StrObj};
    fn check<T>(v: T) -> bool {
        if mem::align_of::<T>() > 8 {
            return false;
        }
        let p = std::rc::Rc::into_raw(std::rc::Rc::new(v));
        let count = unsafe { *((p as *const u8).offset(RC_STRONG as isize) as *const usize) };
        unsafe { std::rc::Rc::increment_strong_count(p) };
        let two = unsafe { *((p as *const u8).offset(RC_STRONG as isize) as *const usize) };
        unsafe {
            std::rc::Rc::decrement_strong_count(p);
            drop(std::rc::Rc::from_raw(p));
        }
        count == 1 && two == 2
    }
    mem::align_of::<ResObj>() <= 8
        && mem::align_of::<crate::value::FuncObj>() <= 8
        && check(StrObj { text: String::new() })
        && check(ListObj { items: Default::default() })
        && check(DictObj { map: Default::default() })
        && check(ObjObj { class: 0, props: Default::default() })
}

/// An inline cache: at a property read or write, the class of the object
/// last seen there and where the property was in it; `check` is what the
/// written value must be (0 anything, 1 a number, 2 a string, 3 a boolean,
/// or 비어있음 for 1-3); `access` who may read it (0 anyone, 1 a method, 2
/// a method of that very object). `class` NONE: nothing seen yet.
#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct Ic {
    class: u32,
    pos: u32,
    check: u32,
    access: u32,
}

const IC_EMPTY: Ic = Ic { class: NONE, pos: 0, check: 0, access: 0 };

pub(super) struct Jit {
    /// What functions with loops in registers are compiled for.
    loop_isa: cranelift_codegen::isa::OwnedTargetIsa,
    /// Whether compiled code counts references itself (`rc_layout_holds`).
    inline_rc: bool,
    /// Where an object's properties are inside it (through the `RefCell`).
    off_props: usize,
    /// Each compiled function's inline caches, one per instruction.
    ics: Vec<Box<[Ic]>>,
    /// Each function's code, or 0 (not compiled): what compiled calls read.
    table: Box<[usize]>,
    /// Calls and loop turns of each function not compiled yet; it is
    /// compiled once they pass `threshold`.
    counts: Vec<u32>,
    threshold: u32,
    /// For each compiled function, which instructions its code does itself:
    /// the interpreter, running some for it, goes on until one of those.
    natives: Vec<Box<[bool]>>,
    module: JITModule,
    ctx: Context,
    fctx: FunctionBuilderContext,
    states: Vec<State>,
}

impl Jit {
    pub(super) fn new(protos: usize, threshold: u32) -> Option<Jit> {
        // Quicker compiles: the code mostly moves values in memory, which the
        // optimizer gains nothing on. (The single-pass register allocator
        // compiles faster still, but its code runs twice as long.)
        let isa_with = |opt: &str| {
            let mut flags = settings::builder();
            flags.set("opt_level", opt).ok()?;
            flags.set("enable_verifier", "false").ok()?;
            flags.set("use_colocated_libcalls", "false").ok()?;
            flags.set("is_pic", "false").ok()?;
            cranelift_native::builder().ok()?.finish(settings::Flags::new(flags)).ok()
        };
        let isa = isa_with(std::env::var("HARU_JIT_OPT").as_deref().unwrap_or("none"))?;
        // Functions with loops in registers: the optimizer takes the checks
        // and moves the copy repeats out of their loops.
        let loop_isa = isa_with("speed")?;
        let module = JITModule::new(JITBuilder::with_isa(isa, cranelift_module::default_libcall_names()));
        let ctx = module.make_context();
        // Compiled code writes `Post::Value` as 0.
        if unsafe { *(&Post::Value as *const Post as *const u32) } != 0 {
            return None;
        }
        Some(Jit {
            loop_isa,
            inline_rc: rc_layout_holds() && std::env::var_os("HARU_JIT_NO_INLINE_RC").is_none(),
            off_props: {
                let o = ObjObj { class: 0, props: Default::default() };
                o.props.as_ptr() as usize - &o as *const ObjObj as usize
            },
            ics: (0..protos).map(|_| Box::default()).collect(),
            table: vec![0; protos].into_boxed_slice(),
            counts: vec![0; protos],
            threshold,
            natives: (0..protos).map(|_| Box::default()).collect(),
            module,
            ctx,
            fctx: FunctionBuilderContext::new(),
            states: (0..protos).map(|_| State::Untried).collect(),
        })
    }

    /// A function's code, counting one call or loop turn of it: compiled
    /// once it is hot.
    fn code(&mut self, id: u32, prog: &Program) -> Option<Code> {
        match self.states[id as usize] {
            State::Ready(c) => Some(c),
            State::Failed => None,
            State::Untried => {
                // Compiling takes time in proportion to the function's size, so a
                // bigger one must have run longer first.
                let size = prog.protos[id as usize].code.len() as u32;
                let n = &mut self.counts[id as usize];
                *n += 1;
                if *n <= self.threshold.saturating_mul(1 + size / 16) {
                    return None;
                }
                // Code that is mostly what the interpreter does anyway gains
                // nothing for the time compiling takes (unless everything is
                // to be compiled: threshold 0).
                let proto = &prog.protos[id as usize];
                if self.threshold > 0 && proto.code.iter().filter(|op| native(op, prog)).count() * 10 < proto.code.len() * 6 {
                    self.states[id as usize] = State::Failed;
                    return None;
                }
                let started = std::time::Instant::now();
                self.ics[id as usize] = vec![IC_EMPTY; prog.protos[id as usize].code.len()].into_boxed_slice();
                let c = self.compile(id, prog);
                if std::env::var_os("HARU_JIT_DEBUG").is_some() {
                    let p = &prog.protos[id as usize];
                    eprintln!("jit: {} ({} ops): {:?}{}", p.name, p.code.len(), started.elapsed(), if c.is_none() { " failed" } else { "" });
                }
                self.states[id as usize] = match c {
                    Some(c) => {
                        self.table[id as usize] = c as usize;
                        self.natives[id as usize] = prog.protos[id as usize].code.iter().map(|op| native(op, prog)).collect();
                        State::Ready(c)
                    }
                    None => State::Failed,
                };
                c
            }
        }
    }

    /// Compiles the function in `ctx` with the optimizer (`loop_isa`) and
    /// hands the code to the module. The code has no relocations: helpers
    /// are called by address and constants live in the code.
    fn define_optimized(&mut self, id: cranelift_module::FuncId) -> Result<(), String> {
        let compiled = self.ctx.compile(&*self.loop_isa, &mut Default::default()).map_err(|e| format!("{:?}", e.inner))?;
        if !compiled.buffer.relocs().is_empty() {
            return Err("relocations".into());
        }
        let align = (compiled.buffer.alignment as u64).max(16);
        let bytes = compiled.code_buffer().to_vec();
        self.module.define_function_bytes(id, align, &bytes, &[]).map_err(|e| format!("{e:?}"))
    }

    fn compile(&mut self, proto_id: u32, prog: &Program) -> Option<Code> {
        let proto = &prog.protos[proto_id as usize];
        if proto.code.is_empty() || std::env::var_os("HARU_JIT_SKIP").is_some_and(|s| s.to_str() == Some(&proto.name)) {
            return None;
        }
        let ptr = self.module.target_config().pointer_type();
        let cc = self.module.isa().default_call_conv();
        self.module.clear_context(&mut self.ctx);
        let sig = &mut self.ctx.func.signature;
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(types::I32));
        let id = self.module.declare_anonymous_function(&self.ctx.func.signature).ok()?;
        let has_loops = {
            let b = FunctionBuilder::new(&mut self.ctx.func, &mut self.fctx);
            let ics = self.ics[proto_id as usize].as_mut_ptr();
            Gen::new(b, ptr, cc, proto, prog, self.table.as_ptr(), self.inline_rc, self.off_props, ics).build()
        };
        if std::env::var_os("HARU_JIT_DEBUG").is_some() {
            eprintln!(
                "  {} blocks, {} insts, {} regs, {}/{} ops native",
                self.ctx.func.dfg.num_blocks(),
                self.ctx.func.dfg.num_insts(),
                proto.nregs,
                proto.code.iter().filter(|op| native(op, prog)).count(),
                proto.code.len()
            );
        }
        let dump = std::env::var_os("HARU_JIT_DUMP").is_some_and(|d| d.to_str() == Some(&proto.name));
        self.ctx.set_disasm(dump);
        let defined = if has_loops && std::env::var_os("HARU_JIT_OPT").is_none() {
            self.define_optimized(id)
        } else {
            self.module.define_function(id, &mut self.ctx).map_err(|e| format!("{e:?}"))
        };
        if dump {
            if let Some(code) = self.ctx.compiled_code().and_then(|c| c.vcode.as_ref()) {
                eprintln!("{code}");
            }
        }
        if let Err(e) = defined {
            if std::env::var_os("HARU_JIT_DEBUG").is_some() {
                eprintln!("jit: {}: {e:?}", proto.name);
            }
            self.module.clear_context(&mut self.ctx);
            return None;
        }
        self.module.clear_context(&mut self.ctx);
        self.module.finalize_definitions().ok()?;
        let p = self.module.get_finalized_function(id);
        Some(unsafe { mem::transmute::<*const u8, Code>(p) })
    }
}

impl Vm<'_> {
    /// The inline cache of instruction `pc` of the running frame `fi`.
    fn ic(&mut self, fi: usize, pc: u32) -> Option<&mut Ic> {
        let proto = self.frames[fi].proto as usize;
        self.jit.as_mut()?.ics.get_mut(proto)?.get_mut(pc as usize)
    }

    /// Whether the interpreter, running instructions for compiled code,
    /// hands back at `pc` of `proto`.
    #[inline]
    pub(super) fn jit_takes_back(&self, proto: u32, pc: usize) -> bool {
        self.jit.as_ref().is_none_or(|j| j.natives[proto as usize].get(pc).copied().unwrap_or(true))
    }

    pub(super) fn jit_code(&mut self, proto: u32) -> Option<Code> {
        let prog = self.prog;
        self.jit.as_mut()?.code(proto, prog)
    }

    /// Where frame `fi`'s code goes on after the interpreter ran something
    /// for it (`r`): a pc, or `RETURNED`, or `ERR` (the signal kept).
    fn jit_resume(&mut self, fi: usize, r: Flow<()>) -> u32 {
        let r = match r {
            // It started a callee's frame: run it to its end.
            Ok(()) if self.frames.len() > fi + 1 => self.jit_run_callee(fi),
            r => r,
        };
        if self.frames.len() <= fi {
            return match r {
                Ok(()) => RETURNED,
                Err(s) => {
                    self.jit_signal = Some(s);
                    ERR
                }
            };
        }
        let ns = self.frames[fi].ns;
        self.ns = ns;
        self.lang = self.namespaces[ns as usize].lang;
        match r {
            Ok(()) => self.frames[fi].pc as u32,
            Err(s) => match self.catch_here(s) {
                Ok(()) => self.frames[fi].pc as u32,
                Err(s) => {
                    self.jit_signal = Some(s);
                    ERR
                }
            },
        }
    }

    /// Runs the frame above `fi` (just started) until it is gone.
    fn jit_run_callee(&mut self, fi: usize) -> Flow<()> {
        let proto = self.frames[fi + 1].proto;
        let saved = self.stop_at;
        self.stop_at = fi + 1;
        let r = match self.jit_code(proto) {
            Some(code) => match invoke(self, code, fi + 1, 0) {
                Done::Returned | Done::End => Ok(()),
                Done::Failed(s) => Err(s),
            },
            None => self.exec(),
        };
        self.stop_at = saved;
        self.jit_finish_callee(fi, r)
    }

    /// The rest of a callee's run after `r`: an error it let out still
    /// unwinds its frame (not frame `fi`'s).
    fn jit_finish_callee(&mut self, fi: usize, mut r: Flow<()>) -> Flow<()> {
        let saved = self.stop_at;
        self.stop_at = fi + 1;
        let r = loop {
            match r {
                Ok(()) => break Ok(()),
                Err(s) if self.frames.len() <= self.stop_at => break Err(s),
                Err(s) => match self.unwind(s) {
                    Ok(()) => r = self.exec(),
                    Err(s) => break Err(s),
                },
            }
        };
        self.stop_at = saved;
        r
    }
}

// ---- counting what the helpers do (HARU_JIT_STATS)

mod stats {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::sync::OnceLock;

    pub fn on() -> bool {
        static ON: OnceLock<bool> = OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("HARU_JIT_STATS").is_some())
    }

    thread_local! {
        static COUNTS: RefCell<HashMap<String, u64>> = RefCell::new(HashMap::new());
    }

    pub fn count(what: impl FnOnce() -> String) {
        if on() {
            COUNTS.with(|c| *c.borrow_mut().entry(what()).or_default() += 1);
        }
    }

    pub fn report() {
        if on() {
            COUNTS.with(|c| {
                let mut v: Vec<_> = c.borrow().iter().map(|(k, n)| (k.clone(), *n)).collect();
                v.sort_by(|a, b| b.1.cmp(&a.1));
                for (k, n) in v {
                    eprintln!("jit stats: {n:>10} {k}");
                }
            });
        }
    }
}

pub(super) fn report_stats() {
    stats::report();
}

/// The instruction at `pc` of frame `fi`, by name (for the counts).
fn op_name(vm: &Vm, fi: usize, pc: u32) -> String {
    let op = vm.prog.protos[vm.frames[fi].proto as usize].code[pc as usize];
    format!("{op:?}").split([' ', '{']).next().unwrap_or("").to_string()
}

// ---- helpers the compiled code calls

unsafe extern "C" fn h_step(env: *mut Env, pc: u32) -> u32 {
    let env = &mut *env;
    let vm = &mut *env.vm;
    let fi = env.fi;
    stats::count(|| format!("step {}", op_name(vm, fi, pc)));
    vm.frames[fi].pc = pc as usize;
    let r = vm.exec_mode::<true>();
    let next = vm.jit_resume(fi, r);
    env.refresh();
    next
}

unsafe extern "C" fn h_call(env: *mut Env, pc: u32) -> u32 {
    stats::count(|| "h_call".to_string());
    let env = &mut *env;
    let vm = &mut *env.vm;
    let fi = env.fi;
    let frame = &vm.frames[fi];
    let base = frame.base;
    let Op::Call { dst, proto, base: b, argc } = vm.prog.protos[frame.proto as usize].code[pc as usize] else {
        unreachable!()
    };
    vm.frames[fi].pc = pc as usize + 1;
    let r = vm.call_plain(proto, base + b as usize, argc, dst);
    let next = vm.jit_resume(fi, r);
    env.refresh();
    next
}

/// A call's fast start: when the callee has native code, starts its frame
/// and fills `callee` for it, returning the code. 0: the callee has none
/// (nothing done); 1: starting it failed (the signal kept).
unsafe extern "C" fn h_call_start(env: *mut Env, pc: u32, callee: *mut Env) -> usize {
    stats::count(|| "h_call_start".to_string());
    let env = &mut *env;
    let vm = &mut *env.vm;
    let fi = env.fi;
    let frame = &vm.frames[fi];
    let base = frame.base;
    let Op::Call { dst, proto, base: b, argc } = vm.prog.protos[frame.proto as usize].code[pc as usize] else {
        unreachable!()
    };
    let Some(code) = vm.jit_code(proto) else {
        return 0;
    };
    vm.frames[fi].pc = pc as usize + 1;
    match vm.call_plain(proto, base + b as usize, argc, dst) {
        Ok(()) => {
            let f = vm.frames.last().unwrap();
            *callee = Env {
                regs: vm.stack.as_mut_ptr().add(f.base),
                globals: vm.globals.as_mut_ptr(),
                vm: env.vm,
                fi: fi + 1,
                argc: argc as usize,
                base: f.base,
                start: 0,
            };
            code as usize
        }
        Err(s) => {
            vm.jit_signal = Some(s);
            env.refresh();
            1
        }
    }
}

/// After a fast call at `pc` failed: to start (`started` 0) or in the callee.
unsafe extern "C" fn h_call_failed(env: *mut Env, pc: u32, started: u32) -> u32 {
    stats::count(|| "h_call_failed".to_string());
    let env = &mut *env;
    let vm = &mut *env.vm;
    let fi = env.fi;
    vm.frames[fi].pc = pc as usize + 1;
    let s = vm.jit_signal.take().expect("a failed call keeps its signal");
    let r = if started != 0 { vm.jit_finish_callee(fi, Err(s)) } else { Err(s) };
    let next = vm.jit_resume(fi, r);
    env.refresh();
    next
}

/// What `h_op` answers when the common case does not hold: it changed
/// nothing, and the interpreter does the instruction instead.
const SLOW: u32 = u32::MAX;

/// Instructions done here directly (no trip through the interpreter's
/// loop), with the interpreter's own code, in their common case: the next
/// pc, or `SLOW`. Nothing is changed before the case is known to hold, so
/// the interpreter can then do the instruction from the start.
unsafe extern "C" fn h_op(env: *mut Env, pc: u32) -> u32 {
    let env = &mut *env;
    let vm = &mut *env.vm;
    let fi = env.fi;
    stats::count(|| format!("op {}", op_name(vm, fi, pc)));
    let prog = vm.prog;
    let op = prog.protos[vm.frames[fi].proto as usize].code[pc as usize];
    let regs = vm.stack.as_mut_ptr().add(env.base);
    let reg = |r: Reg| unsafe { &mut *regs.add(r as usize) };
    let next = pc + 1;
    match op {
        Op::GetVar { dst, var } => match vm.read_var(var, fi) {
            Ok(v) => {
                *reg(dst) = v;
                next
            }
            Err(_) => SLOW,
        },
        Op::Decl { var, src, ty, konst } => match vm.declare(var, fi, reg(src).clone(), ty, konst) {
            Ok(()) => {
                *reg(src) = Value::UNDEF;
                next
            }
            Err(_) => SLOW,
        },
        Op::Assign { var, src } => match vm.assign(var, fi, reg(src).clone()) {
            Ok(()) => {
                *reg(src) = Value::UNDEF;
                next
            }
            Err(_) => SLOW,
        },
        Op::SelfOr { dst, var } => {
            let this = vm.frames[fi].this.clone();
            if !this.is_undef() {
                *reg(dst) = this;
                return next;
            }
            match vm.read_var(var, fi) {
                Ok(v) => {
                    *reg(dst) = v;
                    next
                }
                Err(_) => SLOW,
            }
        }
        Op::GetThis { dst } => {
            let this = vm.frames[fi].this.clone();
            if this.is_undef() {
                return SLOW;
            }
            *reg(dst) = this;
            next
        }
        Op::Member { dst, obj, name, skip } => {
            let o = reg(obj).clone();
            let length = name == vm.length_word;
            match o.tag() {
                tag::LIST if length => {
                    let n = o.as_list().unwrap().items.borrow().len();
                    *reg(dst) = Value::num(n as f64);
                    skip
                }
                tag::STR if length => {
                    let n = o.as_str().unwrap().chars().count();
                    *reg(dst) = Value::num(n as f64);
                    skip
                }
                tag::LIST | tag::STR | tag::DICT => next,
                tag::OBJECT => {
                    let class = o.as_object().unwrap().class;
                    let m = vm.member_of(class, name);
                    if m.getter.is_some() || vm.check_access(fi, &o, m.field_access, false, name).is_err() {
                        return SLOW;
                    }
                    let props = o.as_object().unwrap().props.borrow();
                    let v = props.get(&name).cloned().unwrap_or(Value::NULL);
                    // A field without a getter: compiled code reads it itself
                    // next time (checking who may, as `check_access`).
                    let at = props.position(name);
                    drop(props);
                    let access = match m.field_access {
                        Access::Public => 0,
                        Access::Protected => 1,
                        Access::Private => 2,
                    };
                    if let (Some(pos), Some(ic)) = (at, vm.ic(fi, pc)) {
                        *ic = Ic { class, pos: pos as u32, check: 0, access };
                    }
                    *reg(dst) = v;
                    skip
                }
                CLASS => {
                    let class = o.as_class().unwrap();
                    match vm.namespaces[vm.ns as usize].statics.get(&(class, name)) {
                        Some(v) => {
                            *reg(dst) = v.clone();
                            skip
                        }
                        None => SLOW,
                    }
                }
                _ => SLOW,
            }
        }
        Op::Index { dst, obj, key } => match vm.index(reg(obj), reg(key)) {
            Ok(v) => {
                *reg(dst) = v;
                next
            }
            Err(_) => SLOW,
        },
        Op::IndexK { dst, obj, k } => match vm.index(reg(obj), &prog.consts[k as usize]) {
            Ok(v) => {
                *reg(dst) = v;
                next
            }
            Err(_) => SLOW,
        },
        Op::SetIndex { obj, key, val } => {
            let (o, k, v) = (reg(obj).clone(), reg(key).clone(), reg(val).clone());
            match vm.set_index(&o, k, v) {
                Ok(()) => next,
                Err(_) => SLOW,
            }
        }
        Op::SetIndexK { obj, k, val } => {
            let (o, v) = (reg(obj).clone(), reg(val).clone());
            match vm.set_index(&o, prog.consts[k as usize].clone(), v) {
                Ok(()) => next,
                Err(_) => SLOW,
            }
        }
        Op::SetMember { obj, val, name, skip } => {
            let o = reg(obj).clone();
            match o.tag() {
                tag::LIST | tag::DICT => next,
                tag::OBJECT => {
                    if name == NONE {
                        return skip;
                    }
                    let class = o.as_object().unwrap().class;
                    if vm.member_of(class, name).setter.is_some() {
                        return SLOW;
                    }
                    let v = reg(val).clone();
                    let ty = vm.class(class).and_then(|c| c.field_types.get(&name).copied()).unwrap_or(0);
                    if ty != 0 && !vm.fits(ty, &v) {
                        return SLOW;
                    }
                    let mut props = o.as_object().unwrap().props.borrow_mut();
                    props.insert(name, v);
                    // Compiled code writes it itself next time (checking the
                    // value's type when it is a simple one).
                    let at = props.position(name);
                    drop(props);
                    let check = match ty {
                        0 => Some(0),
                        _ => match prog.types[ty as usize].kind {
                            TypeKind::Any => Some(0),
                            TypeKind::Number => Some(1),
                            TypeKind::String => Some(2),
                            TypeKind::Boolean => Some(3),
                            _ => None,
                        },
                    };
                    if let (Some(pos), Some(check), Some(ic)) = (at, check, vm.ic(fi, pc)) {
                        *ic = Ic { class, pos: pos as u32, check, access: 0 };
                    }
                    skip
                }
                CLASS => {
                    if name != NONE {
                        let v = reg(val).clone();
                        vm.namespaces[vm.ns as usize].statics.insert((o.as_class().unwrap(), name), v);
                    }
                    skip
                }
                tag::STR => SLOW,
                _ => skip,
            }
        }
        Op::MethodPrep { obj, name } => {
            let o = reg(obj).clone();
            match o.tag() {
                tag::OBJECT => {
                    let class = o.as_object().unwrap().class;
                    let m = vm.member_of(class, name);
                    if vm.check_access(fi, &o, m.method_access, true, name).is_err() {
                        return SLOW;
                    }
                    let access = match m.method_access {
                        Access::Public => 0,
                        Access::Protected => 1,
                        Access::Private => 2,
                    };
                    if let Some(ic) = vm.ic(fi, pc) {
                        *ic = Ic { class, pos: 0, check: 0, access };
                    }
                    next
                }
                CLASS => {
                    let class = o.as_class().unwrap();
                    match vm.class(class).is_some_and(|c| c.statics.contains_key(&name)) {
                        true => next,
                        false => SLOW,
                    }
                }
                tag::STR | tag::LIST | tag::RESOURCE => next,
                _ => SLOW,
            }
        }
        Op::Update { var, a, b, op: BinOp::Add } if reg(a).tag() == tag::STR && reg(b).tag() == tag::STR => {
            match vm.append_update(fi, var, a, b) {
                Ok(true) => next,
                _ => SLOW,
            }
        }
        Op::NewObj { dst, class } => {
            crate::gc::safe_point();
            match vm.class(class) {
                Some(c) if !c.is_abstract => {
                    *reg(dst) = Value::object_with(class, c.field_types.len());
                    next
                }
                _ => SLOW,
            }
        }
        Op::InitField { obj, name, src, ty } => {
            if ty != 0 && !vm.fits(ty, reg(src)) {
                return SLOW;
            }
            let v = std::mem::replace(reg(src), Value::UNDEF);
            reg(obj).as_object().unwrap().props.borrow_mut().insert(name, v);
            next
        }
        Op::Format { dst, src } => {
            let text = display(reg(src), vm.lang);
            *reg(dst) = Value::string(text);
            next
        }
        Op::Concat { dst, base: b, n } => {
            let len: usize = (0..n).map(|i| reg(b + i).as_str().map_or(0, str::len)).sum();
            let mut s = String::with_capacity(len);
            for i in 0..n {
                s.push_str(reg(b + i).as_str().unwrap_or(""));
            }
            *reg(dst) = Value::string(s);
            next
        }
        Op::IterNext { iter, idx, dst, exit } => {
            let i = reg(idx).as_num().unwrap() as usize;
            let item = reg(iter).as_list().unwrap().items.borrow().get(i).cloned();
            match item {
                Some(v) => {
                    *reg(dst) = v;
                    *reg(idx) = Value::num((i + 1) as f64);
                    next
                }
                None => exit,
            }
        }
        Op::ListCheck { list, target } => {
            if reg(list).tag() != tag::LIST || vm.require_mutable(target, fi).is_err() {
                return SLOW;
            }
            next
        }
        Op::ListPush { list, val, front, target } => {
            let l = reg(list).clone();
            let Some(items) = l.as_list().map(|l| &l.items) else {
                return SLOW;
            };
            // A copy goes in: when the check fails it comes out again and the
            // interpreter does it all with the register as it was.
            let v = reg(val).clone();
            if front {
                items.borrow_mut().insert(0, v);
            } else {
                items.borrow_mut().push(v);
            }
            if vm.check_push(target, fi, &l, front).is_err() {
                if front {
                    items.borrow_mut().remove(0);
                } else {
                    items.borrow_mut().pop();
                }
                return SLOW;
            }
            *reg(val) = Value::UNDEF;
            next
        }
        Op::Eq { dst, a, b, neg } => {
            // An object whose class has `<기호 같다>` decides itself.
            if let Some(o) = reg(a).as_object() {
                if vm.class(o.class).and_then(|c| c.equals).is_some() {
                    return SLOW;
                }
            }
            let eq = reg(a).go_eq(reg(b));
            *reg(dst) = Value::bool(eq != neg);
            next
        }
        _ => SLOW,
    }
}

/// `h_call_start` for a constructor (`새로운`): its result is dropped.
unsafe extern "C" fn h_ctor_start(env: *mut Env, pc: u32, callee: *mut Env) -> usize {
    stats::count(|| "h_ctor_start".to_string());
    let env = &mut *env;
    let vm = &mut *env.vm;
    let fi = env.fi;
    let frame = &vm.frames[fi];
    let base = frame.base;
    let Op::CallCtor { obj, proto, class, base: b, argc } = vm.prog.protos[frame.proto as usize].code[pc as usize] else {
        unreachable!()
    };
    let Some(code) = vm.jit_code(proto) else {
        return 0;
    };
    let o = vm.stack[base + obj as usize].clone();
    vm.frames[fi].pc = pc as usize + 1;
    match vm.call_proto(proto, base + b as usize, argc, obj, o, class, Post::Discard, true) {
        Ok(()) => {
            let f = vm.frames.last().unwrap();
            *callee = Env {
                regs: vm.stack.as_mut_ptr().add(f.base),
                globals: vm.globals.as_mut_ptr(),
                vm: env.vm,
                fi: fi + 1,
                argc: argc as usize,
                base: f.base,
                start: 0,
            };
            code as usize
        }
        Err(s) => {
            vm.jit_signal = Some(s);
            env.refresh();
            1
        }
    }
}

/// `h_call_start` for a method or a static method of the program.
unsafe extern "C" fn h_method_start(env: *mut Env, pc: u32, callee: *mut Env) -> usize {
    stats::count(|| "h_method_start".to_string());
    let env = &mut *env;
    let vm = &mut *env.vm;
    let fi = env.fi;
    let frame = &vm.frames[fi];
    let base = frame.base;
    let Op::CallMethod { dst, obj, name, base: b, argc, .. } = vm.prog.protos[frame.proto as usize].code[pc as usize] else {
        unreachable!()
    };
    let o = vm.stack[base + obj as usize].clone();
    let (proto, this, class) = match o.tag() {
        tag::OBJECT => {
            let class = o.as_object().unwrap().class;
            let p = if name == vm.init_name { vm.class(class).and_then(|c| c.ctor) } else { vm.member_of(class, name).method };
            (p, o, class)
        }
        CLASS => {
            let class = o.as_class().unwrap();
            (vm.class(class).and_then(|c| c.statics.get(&name).copied()), Value::UNDEF, class)
        }
        _ => return 0,
    };
    let Some(proto) = proto else {
        return 0;
    };
    let Some(code) = vm.jit_code(proto) else {
        return 0;
    };
    vm.frames[fi].pc = pc as usize + 1;
    match vm.call_proto(proto, base + b as usize, argc, dst, this, class, Post::Value, true) {
        Ok(()) => {
            let f = vm.frames.last().unwrap();
            *callee = Env {
                regs: vm.stack.as_mut_ptr().add(f.base),
                globals: vm.globals.as_mut_ptr(),
                vm: env.vm,
                fi: fi + 1,
                argc: argc as usize,
                base: f.base,
                start: 0,
            };
            code as usize
        }
        Err(s) => {
            vm.jit_signal = Some(s);
            env.refresh();
            1
        }
    }
}

unsafe extern "C" fn h_return(env: *mut Env, src: u32) -> u32 {
    stats::count(|| "h_return".to_string());
    let env = &mut *env;
    let vm = &mut *env.vm;
    let v = mem::replace(&mut vm.stack[env.base + src as usize], Value::UNDEF);
    match vm.finish_call(v, false) {
        Ok(()) => S_RETURNED,
        Err(s) => {
            vm.jit_signal = Some(s);
            S_ERR
        }
    }
}

unsafe extern "C" fn h_return_null(env: *mut Env) -> u32 {
    stats::count(|| "h_return_null".to_string());
    let env = &mut *env;
    let vm = &mut *env.vm;
    if env.fi == 0 {
        return S_END;
    }
    match vm.finish_call(Value::NULL, true) {
        Ok(()) => S_RETURNED,
        Err(s) => {
            vm.jit_signal = Some(s);
            S_ERR
        }
    }
}

unsafe extern "C" fn h_drop(v: *mut Value) {
    stats::count(|| "h_drop".to_string());
    std::ptr::drop_in_place(v);
}

unsafe extern "C" fn h_clone_into(dst: *mut Value, src: *const Value) {
    stats::count(|| "h_clone_into".to_string());
    *dst = (*src).clone();
}

// ---- code generation

// Where compiled code finds what it reads and writes directly.
const OFF_STACK: usize = mem::offset_of!(Vm<'static>, stack);
const OFF_FRAMES: usize = mem::offset_of!(Vm<'static>, frames);
const OFF_DEPTH: usize = mem::offset_of!(Vm<'static>, depth);
const OFF_NS: usize = mem::offset_of!(Vm<'static>, ns);
const OFF_PTR: usize = mem::offset_of!(Stack<Value>, ptr);
const OFF_LEN: usize = mem::offset_of!(Stack<Value>, len);
const OFF_CAP: usize = mem::offset_of!(Stack<Value>, cap);
const FRAME_SIZE: usize = mem::size_of::<Frame>();
const F_PROTO: usize = mem::offset_of!(Frame, proto);
const F_PC: usize = mem::offset_of!(Frame, pc);
const F_BASE: usize = mem::offset_of!(Frame, base);
const F_ARGC: usize = mem::offset_of!(Frame, argc);
const F_DEPTH: usize = mem::offset_of!(Frame, depth);
const F_COUNTED: usize = mem::offset_of!(Frame, counted);
const F_RET: usize = mem::offset_of!(Frame, ret);
const F_POST: usize = mem::offset_of!(Frame, post);
const F_THIS: usize = mem::offset_of!(Frame, this);
const F_SELF_CLASS: usize = mem::offset_of!(Frame, self_class);
const F_NS: usize = mem::offset_of!(Frame, ns);
const F_PENDING: usize = mem::offset_of!(Frame, pending);
const OFF_OBJ_CLASS: usize = mem::offset_of!(ObjObj, class);
const PROP_SIZE: usize = mem::size_of::<crate::value::Prop>();
const PROP_NAME: usize = mem::offset_of!(crate::value::Prop, name);
const PROP_VALUE: usize = mem::offset_of!(crate::value::Prop, value);

const TAG_NULL: i64 = tag::NULL as i64;
const TAG_BOOL: i64 = tag::BOOL as i64;
const TAG_NUM: i64 = tag::NUM as i64;
const TAG_UNDEF: i64 = crate::value::UNDEF as i64;

struct Sigs {
    step: SigRef,
    ret: SigRef,
    start: SigRef,
    failed: SigRef,
    native: SigRef,
    env_only: SigRef,
    one_ptr: SigRef,
    two_ptr: SigRef,
}

struct Gen<'a, 'b> {
    b: FunctionBuilder<'b>,
    ptr: types::Type,
    proto: &'a Proto,
    prog: &'a Program,
    env: V,
    /// The VM, the frame's index and its first register's offset (bytes).
    vm: V,
    fi: V,
    base_off: V,
    table: *const usize,
    inline_rc: bool,
    off_props: usize,
    ics: *mut Ic,
    /// The registers' address (loaded again after anything that may have
    /// moved the stack).
    regs: Variable,
    /// The globals' address (they never move).
    globals: V,
    blocks: Vec<Block>,
    dispatch: Block,
    trap: Block,
    sigs: Sigs,
}

/// Where a value sits: a register or a global.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum At {
    Reg(Reg),
    Global(u32),
}

fn flags() -> MemFlags {
    MemFlags::trusted()
}

impl<'a, 'b> Gen<'a, 'b> {
    fn new(
        mut b: FunctionBuilder<'b>,
        ptr: types::Type,
        cc: cranelift_codegen::isa::CallConv,
        proto: &'a Proto,
        prog: &'a Program,
        table: *const usize,
        inline_rc: bool,
        off_props: usize,
        ics: *mut Ic,
    ) -> Self {
        let mut sig = |params: &[types::Type], ret: Option<types::Type>| {
            let mut s = Signature::new(cc);
            s.params.extend(params.iter().map(|t| AbiParam::new(*t)));
            s.returns.extend(ret.map(AbiParam::new));
            b.import_signature(s)
        };
        let sigs = Sigs {
            step: sig(&[ptr, types::I32], Some(types::I32)),
            ret: sig(&[ptr, types::I32], Some(types::I32)),
            start: sig(&[ptr, types::I32, ptr], Some(ptr)),
            failed: sig(&[ptr, types::I32, types::I32], Some(types::I32)),
            native: sig(&[ptr], Some(types::I32)),
            env_only: sig(&[ptr], Some(types::I32)),
            one_ptr: sig(&[ptr], None),
            two_ptr: sig(&[ptr, ptr], None),
        };
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        let blocks: Vec<Block> = proto.code.iter().map(|_| b.create_block()).collect();
        let dispatch = b.create_block();
        b.append_block_param(dispatch, types::I32);
        let trap = b.create_block();
        b.switch_to_block(entry);
        let env = b.block_params(entry)[0];
        let vm = b.ins().load(ptr, flags(), env, 16);
        let fi = b.ins().load(ptr, flags(), env, 24);
        let base = b.ins().load(ptr, flags(), env, 40);
        let base_off = b.ins().ishl_imm(base, 4);
        let regs = b.declare_var(ptr);
        let globals = b.ins().load(ptr, flags(), env, 8);
        let mut g = Gen {
            b,
            ptr,
            proto,
            prog,
            env,
            vm,
            fi,
            base_off,
            table,
            inline_rc,
            off_props,
            ics,
            regs,
            globals,
            blocks,
            dispatch,
            trap,
            sigs,
        };
        g.reload();
        let start = g.b.ins().load(types::I64, flags(), g.env, 48);
        let at = g.b.ins().ireduce(types::I32, start);
        let (first, dispatch) = (g.blocks[0], g.dispatch);
        g.b.ins().brif(start, dispatch, &[BlockArg::Value(at)], first, &[]);
        g
    }

    /// Builds the function; whether it has loops in registers.
    fn build(mut self) -> bool {
        // Loops of numbers get a copy that keeps them in CPU registers,
        // entered at the loop head.
        let found = match std::env::var_os("HARU_JIT_NO_LOOPS") {
            Some(_) => Vec::new(),
            None => loops::find(&self.proto.code, self.prog),
        };
        let mut regions: Vec<loops::Region> = found.iter().map(|&(h, j)| self.region(h, j)).collect();
        if std::env::var_os("HARU_JIT_DEBUG").is_some() && !found.is_empty() {
            eprintln!("  loops in registers: {found:?}");
        }
        for i in 0..self.proto.code.len() {
            let blk = self.blocks[i];
            self.switch(blk);
            if let Some(r) = regions.iter().find(|r| r.head == i) {
                let ordinary = self.b.create_block();
                self.enter_region(r, ordinary);
                self.switch(ordinary);
            }
            self.op(i);
        }
        for r in &mut regions {
            self.emit_region(r);
        }
        let has_loops = !regions.is_empty();
        self.emit_dispatch();
        self.switch(self.trap);
        self.b.ins().trap(TrapCode::unwrap_user(1));
        self.b.seal_all_blocks();
        self.b.finalize();
        has_loops
    }

    /// The registers' address, again (after a helper that may have moved
    /// the stack).
    fn reload(&mut self) {
        let sp = self.b.ins().load(self.ptr, flags(), self.vm, (OFF_STACK + OFF_PTR) as i32);
        let r = self.b.ins().iadd(sp, self.base_off);
        self.b.def_var(self.regs, r);
    }

    fn switch(&mut self, blk: Block) {
        self.b.switch_to_block(blk);
    }

    fn addr(&mut self, at: At) -> V {
        let base = match at {
            At::Reg(_) => self.b.use_var(self.regs),
            At::Global(_) => self.globals,
        };
        let i = match at {
            At::Reg(r) => r as i64,
            At::Global(g) => g as i64,
        };
        self.b.ins().iadd_imm(base, i * 16)
    }

    fn tag_of(&mut self, a: V) -> V {
        self.b.ins().load(types::I32, flags(), a, 0)
    }

    fn num_of(&mut self, a: V) -> V {
        self.b.ins().load(types::F64, flags(), a, 8)
    }

    fn payload_of(&mut self, a: V) -> V {
        self.b.ins().load(types::I64, flags(), a, 8)
    }

    fn is_tag(&mut self, t: V, want: i64) -> V {
        self.b.ins().icmp_imm(IntCC::Equal, t, want)
    }

    fn call(&mut self, sig: SigRef, f: usize, args: &[V]) -> Option<V> {
        let callee = self.b.ins().iconst(self.ptr, f as i64);
        let inst = self.b.ins().call_indirect(sig, callee, args);
        self.b.inst_results(inst).first().copied()
    }

    /// Writes a value to `a`, releasing what was there.
    fn store(&mut self, a: V, tag: V, payload: V) {
        self.release(a);
        let tag = if self.b.func.dfg.value_type(tag) == types::I64 { tag } else { self.b.ins().uextend(types::I64, tag) };
        // Tag and (zero) padding in one store.
        self.b.ins().store(flags(), tag, a, 0);
        let payload = if self.b.func.dfg.value_type(payload) == types::F64 {
            self.b.ins().bitcast(types::I64, MemFlags::new(), payload)
        } else {
            payload
        };
        self.b.ins().store(flags(), payload, a, 8);
    }

    fn store_num(&mut self, a: V, n: V) {
        let t = self.b.ins().iconst(types::I64, TAG_NUM);
        self.store(a, t, n);
    }

    fn store_bool(&mut self, a: V, cond: V) {
        let t = self.b.ins().iconst(types::I64, TAG_BOOL);
        let p = self.b.ins().uextend(types::I64, cond);
        self.store(a, t, p);
    }

    /// Hana's `num.Box`: -0 becomes 0.
    fn boxed(&mut self, n: V) -> V {
        let zero = self.b.ins().f64const(0.0);
        let is_zero = self.b.ins().fcmp(FloatCC::Equal, n, zero);
        self.b.ins().select(is_zero, zero, n)
    }

    fn next(&self, i: usize) -> Block {
        self.blocks.get(i + 1).copied().unwrap_or(self.trap)
    }

    /// Hands instruction `i` to the interpreter and goes where it says.
    fn step(&mut self, i: usize) {
        let pc = self.b.ins().iconst(types::I32, i as i64);
        let r = self.call(self.sigs.step, h_step as usize, &[self.env, pc]).unwrap();
        self.go_on(i, r);
    }

    /// After a helper answered `r` for instruction `i`.
    fn go_on(&mut self, i: usize, r: V) {
        self.reload();
        let is_next = self.b.ins().icmp_imm(IntCC::Equal, r, i as i64 + 1);
        let next = self.next(i);
        self.b.ins().brif(is_next, next, &[], self.dispatch, &[BlockArg::Value(r)]);
    }

    /// A block that runs `i` through the interpreter (a fast path's way out).
    fn slow_block(&mut self, i: usize) -> Block {
        let cur = self.b.current_block().unwrap();
        let slow = self.b.create_block();
        self.switch(slow);
        self.step(i);
        self.switch(cur);
        slow
    }

    fn emit_dispatch(&mut self) {
        self.switch(self.dispatch);
        let r = self.b.block_params(self.dispatch)[0];
        let special = self.b.ins().icmp_imm(IntCC::UnsignedGreaterThanOrEqual, r, RETURNED as i64);
        let leave = self.b.create_block();
        let table = self.b.create_block();
        self.b.ins().brif(special, leave, &[], table, &[]);

        self.switch(leave);
        let status = self.b.ins().iadd_imm(r, -(RETURNED as i64));
        self.b.ins().return_(&[status]);

        self.switch(table);
        let default = self.b.func.dfg.block_call(self.trap, &[]);
        let targets: Vec<_> = self.blocks.clone().into_iter().map(|blk| self.b.func.dfg.block_call(blk, &[])).collect();
        let jt = self.b.create_jump_table(JumpTableData::new(default, &targets));
        self.b.ins().br_table(r, jt);
    }

    /// A number constant's value, if `k` is one.
    fn const_num(&self, k: u32) -> Option<f64> {
        self.prog.consts[k as usize].as_num()
    }

    fn op(&mut self, i: usize) {
        match self.proto.code[i] {
            Op::LoadK { dst, k } => {
                let c = &self.prog.consts[k as usize];
                let (t, p) = match (c.tag(), c.as_num(), c.as_bool()) {
                    (tag::NUM, Some(n), _) => (TAG_NUM, n.to_bits() as i64),
                    (tag::BOOL, _, Some(v)) => (TAG_BOOL, v as i64),
                    (tag::NULL, _, _) => (TAG_NULL, 0),
                    _ => {
                        // A string: a new reference to the constant.
                        let d = self.addr(At::Reg(dst));
                        let k = self.b.ins().iconst(self.ptr, c as *const Value as i64);
                        if self.inline_rc && matches!(c.tag(), tag::STR..=tag::RESOURCE) {
                            self.retain_counted(k);
                            let t = self.b.ins().iconst(types::I64, c.tag() as i64);
                            let p = self.payload_of(k);
                            self.store(d, t, p);
                        } else {
                            self.call(self.sigs.two_ptr, h_clone_into as usize, &[d, k]);
                        }
                        return self.jump_next(i);
                    }
                };
                let a = self.addr(At::Reg(dst));
                let t = self.b.ins().iconst(types::I64, t);
                let p = self.b.ins().iconst(types::I64, p);
                self.store(a, t, p);
                self.jump_next(i);
            }
            Op::LoadNull { dst } => {
                let a = self.addr(At::Reg(dst));
                let t = self.b.ins().iconst(types::I64, TAG_NULL);
                let p = self.b.ins().iconst(types::I64, 0);
                self.store(a, t, p);
                self.jump_next(i);
            }
            Op::LoadBool { dst, v } => {
                let a = self.addr(At::Reg(dst));
                let t = self.b.ins().iconst(types::I64, TAG_BOOL);
                let p = self.b.ins().iconst(types::I64, v as i64);
                self.store(a, t, p);
                self.jump_next(i);
            }
            Op::Move { dst, src } => self.copy(i, At::Reg(dst), At::Reg(src), false),
            Op::GetReg { dst, slot, .. } => self.copy(i, At::Reg(dst), At::Reg(slot), true),
            Op::GetGlobal { dst, slot, .. } => self.copy(i, At::Reg(dst), At::Global(slot), true),
            Op::SetReg { slot, src } => self.take(i, At::Reg(slot), src),
            Op::SetGlobal { slot, src } => self.take(i, At::Global(slot), src),
            Op::Bin { op, dst, a, b } => self.binary(i, op, dst, a, Some(b), None),
            Op::BinK { op, dst, a, k } => match self.const_num(k) {
                Some(y) => self.binary(i, op, dst, a, None, Some(y)),
                None => self.step(i),
            },
            Op::Update { op, .. } if !matches!(op, BinOp::Add | BinOp::Sub) => self.direct(i),
            Op::Update { var, a, b, op } => {
                let (target, meta) = match self.prog.vars[var as usize].slots.as_slice() {
                    [Slot { loc: Loc::Reg(r), meta }] => (At::Reg(*r), *meta),
                    [Slot { loc: Loc::Global(g), meta }] => (At::Global(*g), *meta),
                    _ => return self.direct(i),
                };
                // Strings (appending) and the rest: h_op, else the interpreter.
                let slow = self.direct_block(i);
                if let Some(m) = meta {
                    // A declared type or 고정: a number fits when the type is
                    // none, a number or anything, and it is not a constant.
                    let m = match m {
                        Loc::Reg(r) => At::Reg(r),
                        Loc::Global(g) => At::Global(g),
                        Loc::This(_) => return self.direct(i),
                    };
                    let ma = self.addr(m);
                    let t = self.tag_of(ma);
                    let is_num = self.is_tag(t, TAG_NUM);
                    let (typed, untyped) = (self.b.create_block(), self.b.create_block());
                    let not_num = self.b.create_block();
                    self.b.ins().brif(is_num, typed, &[], not_num, &[]);
                    self.switch(not_num);
                    // Undefined or null: no type.
                    let u = self.is_tag(t, TAG_UNDEF);
                    let n = self.is_tag(t, TAG_NULL);
                    let none = self.b.ins().bor(u, n);
                    self.b.ins().brif(none, untyped, &[], slow, &[]);
                    self.switch(typed);
                    let v = self.num_of(ma);
                    let mut ok = None;
                    for id in number_types(self.prog) {
                        let k = self.b.ins().f64const((id * 2) as f64);
                        let c = self.b.ins().fcmp(FloatCC::Equal, v, k);
                        ok = Some(match ok {
                            Some(o) => self.b.ins().bor(o, c),
                            None => c,
                        });
                    }
                    match ok {
                        Some(ok) => self.b.ins().brif(ok, untyped, &[], slow, &[]),
                        None => self.b.ins().jump(slow, &[]),
                    };
                    self.switch(untyped);
                }
                let (xa, ya) = (self.addr(At::Reg(a)), self.addr(At::Reg(b)));
                let (x, y) = self.both_nums(xa, ya, slow);
                let r = if op == BinOp::Add { self.b.ins().fadd(x, y) } else { self.b.ins().fsub(x, y) };
                let r = self.boxed(r);
                let t = self.addr(target);
                self.store_num(t, r);
                self.jump_next(i);
            }
            Op::Undef { from, to } => {
                for r in from..to {
                    let a = self.addr(At::Reg(r));
                    let t = self.b.ins().iconst(types::I64, TAG_UNDEF);
                    let p = self.b.ins().iconst(types::I64, 0);
                    self.store(a, t, p);
                }
                self.jump_next(i);
            }
            Op::Eq { dst, a, b, neg } => {
                let slow = self.direct_block(i);
                let (xa, ya) = (self.addr(At::Reg(a)), self.addr(At::Reg(b)));
                let (x, y) = self.both_nums(xa, ya, slow);
                let cc = if neg { FloatCC::NotEqual } else { FloatCC::Equal };
                let c = self.b.ins().fcmp(cc, x, y);
                let d = self.addr(At::Reg(dst));
                self.store_bool(d, c);
                self.jump_next(i);
            }
            Op::Truth { dst, src } => {
                let slow = self.slow_block(i);
                let s = self.addr(At::Reg(src));
                let t = self.tag_of(s);
                let ok = self.is_tag(t, TAG_BOOL);
                let fast = self.b.create_block();
                self.b.ins().brif(ok, fast, &[], slow, &[]);
                self.switch(fast);
                let p = self.payload_of(s);
                let t = self.b.ins().iconst(types::I64, TAG_BOOL);
                let d = self.addr(At::Reg(dst));
                self.store(d, t, p);
                self.jump_next(i);
            }
            Op::Jump { to } => {
                let blk = self.blocks[to as usize];
                self.b.ins().jump(blk, &[]);
            }
            Op::JumpIfFalse { cond, to } | Op::JumpIfTrue { cond, to } => {
                let jump_on = matches!(self.proto.code[i], Op::JumpIfTrue { .. });
                let slow = self.slow_block(i);
                let c = self.addr(At::Reg(cond));
                let t = self.tag_of(c);
                let ok = self.is_tag(t, TAG_BOOL);
                let fast = self.b.create_block();
                self.b.ins().brif(ok, fast, &[], slow, &[]);
                self.switch(fast);
                let p = self.payload_of(c);
                let (to, next) = (self.blocks[to as usize], self.next(i));
                if jump_on {
                    self.b.ins().brif(p, to, &[], next, &[]);
                } else {
                    self.b.ins().brif(p, next, &[], to, &[]);
                }
            }
            Op::CmpJump { op, a, b, to } => self.cmp_jump(i, op, a, Some(b), None, to),
            Op::CmpKJump { op, a, k, to } => match self.const_num(k) {
                Some(y) => self.cmp_jump(i, op, a, None, Some(y), to),
                None => self.step(i),
            },
            Op::RangePrep { start, end, step } => {
                let slow = self.slow_block(i);
                let (sa, ea) = (self.addr(At::Reg(start)), self.addr(At::Reg(end)));
                let (s, e) = self.both_nums(sa, ea, slow);
                let down = self.b.ins().fcmp(FloatCC::GreaterThan, s, e);
                let (m1, p1) = (self.b.ins().f64const(-1.0), self.b.ins().f64const(1.0));
                let v = self.b.ins().select(down, m1, p1);
                let d = self.addr(At::Reg(step));
                self.store_num(d, v);
                self.jump_next(i);
            }
            Op::RangeTest { v, end, step, exit } => {
                let (va, ea, sa) = (self.addr(At::Reg(v)), self.addr(At::Reg(end)), self.addr(At::Reg(step)));
                let (v, e, s) = (self.num_of(va), self.num_of(ea), self.num_of(sa));
                let zero = self.b.ins().f64const(0.0);
                let up = self.b.ins().fcmp(FloatCC::GreaterThan, s, zero);
                let past_up = self.b.ins().fcmp(FloatCC::GreaterThan, v, e);
                let a = self.b.ins().band(up, past_up);
                let down = self.b.ins().fcmp(FloatCC::LessThan, s, zero);
                let past_down = self.b.ins().fcmp(FloatCC::LessThan, v, e);
                let c = self.b.ins().band(down, past_down);
                let out = self.b.ins().bor(a, c);
                let (exit, next) = (self.blocks[exit as usize], self.next(i));
                self.b.ins().brif(out, exit, &[], next, &[]);
            }
            Op::RangeStep { v, step } => {
                let (va, sa) = (self.addr(At::Reg(v)), self.addr(At::Reg(step)));
                let (x, s) = (self.num_of(va), self.num_of(sa));
                let n = self.b.ins().fadd(x, s);
                self.b.ins().store(flags(), n, va, 8);
                self.jump_next(i);
            }
            Op::Boxed { dst } => {
                let a = self.addr(At::Reg(dst));
                let t = self.tag_of(a);
                let is_num = self.is_tag(t, TAG_NUM);
                let fast = self.b.create_block();
                let next = self.next(i);
                self.b.ins().brif(is_num, fast, &[], next, &[]);
                self.switch(fast);
                let n = self.num_of(a);
                let n = self.boxed(n);
                self.b.ins().store(flags(), n, a, 8);
                self.jump_next(i);
            }
            Op::Enter => {
                let slow = self.slow_block(i);
                let d = self.b.ins().load(types::I32, flags(), self.vm, OFF_DEPTH as i32);
                let full = self.b.ins().icmp_imm(IntCC::UnsignedGreaterThanOrEqual, d, MAX_CALL_DEPTH as i64);
                let fast = self.b.create_block();
                self.b.ins().brif(full, slow, &[], fast, &[]);
                self.switch(fast);
                let d = self.b.ins().iadd_imm(d, 1);
                self.b.ins().store(flags(), d, self.vm, OFF_DEPTH as i32);
                self.jump_next(i);
            }
            Op::Leave => {
                let d = self.b.ins().load(types::I32, flags(), self.vm, OFF_DEPTH as i32);
                let d = self.b.ins().iadd_imm(d, -1);
                self.b.ins().store(flags(), d, self.vm, OFF_DEPTH as i32);
                self.jump_next(i);
            }
            Op::Call { dst, proto, base, argc } => match self.inline_call(proto, argc) {
                true => self.fast_call(i, dst, proto, base, argc),
                false => self.call_op(i),
            },
            Op::CallMethod { .. } => self.call_via(i, h_method_start as usize, h_step as usize),
            Op::SelfOr { dst, .. } => self.self_or(i, dst),
            Op::MethodPrep { obj, .. } => self.method_prep(i, obj),
            Op::GetVar { dst, var } => match self.prog.vars[var as usize].slots.first() {
                Some(Slot { loc: Loc::Reg(r), .. }) => self.get_var_reg(i, dst, *r),
                _ => self.direct(i),
            },
            Op::Member { dst, obj, name, skip } => self.member(i, dst, obj, name, skip),
            Op::SetMember { obj, val, name, skip } if name != NONE => self.set_member(i, obj, val, name, skip),
            Op::SetMember { .. } => self.direct(i),
            Op::CallCtor { .. } => self.call_via(i, h_ctor_start as usize, h_step as usize),
            Op::Decl { .. }
            | Op::Assign { .. }
            | Op::GetThis { .. }
            | Op::Index { .. }
            | Op::IndexK { .. }
            | Op::SetIndex { .. }
            | Op::SetIndexK { .. }
            | Op::Format { .. }
            | Op::Concat { .. }
            | Op::IterNext { .. }
            | Op::ListCheck { .. }
            | Op::ListPush { .. }
            | Op::NewObj { .. }
            | Op::InitField { .. } => self.direct(i),
            Op::ArgGiven { index, skip } => {
                let argc = self.b.ins().load(types::I64, flags(), self.env, 32);
                let given = self.b.ins().icmp_imm(IntCC::UnsignedGreaterThan, argc, index as i64);
                let (skip, next) = (self.blocks[skip as usize], self.next(i));
                self.b.ins().brif(given, skip, &[], next, &[]);
            }
            Op::Return { src } => self.return_op(Some(src)),
            Op::ReturnNull => self.return_op(None),
            _ => self.step(i),
        }
    }

    /// The tags a type annotation lets through without a closer look.
    fn simple_type(&self, ty: u32) -> Option<&'static [i64]> {
        if ty == 0 {
            return Some(&[]);
        }
        match self.prog.types[ty as usize].kind {
            TypeKind::Any => Some(&[]),
            TypeKind::Number => Some(&[TAG_NUM, TAG_NULL]),
            TypeKind::String => Some(&[tag::STR as i64, TAG_NULL]),
            TypeKind::Boolean => Some(&[TAG_BOOL, TAG_NULL]),
            _ => None,
        }
    }

    /// Whether a call of `proto` with `argc` arguments can start its frame
    /// here (else the helpers start it).
    fn inline_call(&self, proto: u32, argc: u16) -> bool {
        let p = &self.prog.protos[proto as usize];
        !p.raw_params
            && argc as usize <= p.params.len()
            && p.params.iter().take(argc as usize).all(|q| self.simple_type(q.ty).is_some())
    }

    /// Goes to `slow` unless the tag at `a` is one of `tags` (none: any).
    fn check_tags(&mut self, a: V, tags: &[i64], slow: Block) {
        if tags.is_empty() {
            return;
        }
        let t = self.tag_of(a);
        let mut ok = self.is_tag(t, tags[0]);
        for &x in &tags[1..] {
            let o = self.is_tag(t, x);
            ok = self.b.ins().bor(ok, o);
        }
        let fast = self.b.create_block();
        self.b.ins().brif(ok, fast, &[], slow, &[]);
        self.switch(fast);
    }

    /// A call of a function of the program, its frame started here: the
    /// checks `call_proto` makes, then the frame, then its native code.
    /// Anything unusual (no code yet, a full stack, a wrong type) takes the
    /// helpers' way, which does all of it again.
    fn fast_call(&mut self, i: usize, dst: Reg, proto: u32, b: Reg, argc: u16) {
        let callee = &self.prog.protos[proto as usize];
        let nregs = callee.nregs as i64;
        let slow = self.b.create_block();
        let entry = self.b.ins().iconst(self.ptr, unsafe { self.table.add(proto as usize) } as i64);
        let code = self.b.ins().load(self.ptr, MemFlags::new(), entry, 0);
        let fast = self.b.create_block();
        self.b.ins().brif(code, fast, &[], slow, &[]);
        self.switch(fast);
        for (k, q) in callee.params.iter().enumerate().take(argc as usize) {
            let tags = self.simple_type(q.ty).unwrap();
            let a = self.addr(At::Reg(b + k as Reg));
            self.check_tags(a, tags, slow);
        }
        let (vm, ptr) = (self.vm, self.ptr);
        let slen = self.b.ins().load(ptr, flags(), vm, (OFF_STACK + OFF_LEN) as i32);
        let scap = self.b.ins().load(ptr, flags(), vm, (OFF_STACK + OFF_CAP) as i32);
        let need = self.b.ins().iadd_imm(slen, nregs);
        let room = self.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, need, scap);
        let flen = self.b.ins().load(ptr, flags(), vm, (OFF_FRAMES + OFF_LEN) as i32);
        let fcap = self.b.ins().load(ptr, flags(), vm, (OFF_FRAMES + OFF_CAP) as i32);
        let froom = self.b.ins().icmp(IntCC::UnsignedLessThan, flen, fcap);
        let both = self.b.ins().band(room, froom);
        let go = self.b.create_block();
        self.b.ins().brif(both, go, &[], slow, &[]);

        // Nothing can fail from here on.
        self.switch(go);
        let sp = self.b.ins().load(ptr, flags(), vm, (OFF_STACK + OFF_PTR) as i32);
        let off = self.b.ins().ishl_imm(slen, 4);
        let nr = self.b.ins().iadd(sp, off);
        let undef = self.b.ins().iconst(types::I64, TAG_UNDEF);
        let zero = self.b.ins().iconst(types::I64, 0);
        for r in 0..nregs {
            self.b.ins().store(flags(), undef, nr, (r * 16) as i32);
            self.b.ins().store(flags(), zero, nr, (r * 16 + 8) as i32);
        }
        for (k, q) in callee.params.iter().enumerate().take(argc as usize) {
            let a = self.addr(At::Reg(b + k as Reg));
            let t = self.b.ins().load(types::I64, flags(), a, 0);
            let p = self.b.ins().load(types::I64, flags(), a, 8);
            self.b.ins().store(flags(), t, nr, q.slot as i32 * 16);
            self.b.ins().store(flags(), p, nr, q.slot as i32 * 16 + 8);
            self.b.ins().store(flags(), undef, a, 0);
            if let Some(m) = q.meta {
                // The declared type, as `meta_value` writes it.
                let t = self.b.ins().iconst(types::I64, TAG_NUM);
                let n = self.b.ins().iconst(types::I64, ((q.ty * 2) as f64).to_bits() as i64);
                self.b.ins().store(flags(), t, nr, m as i32 * 16);
                self.b.ins().store(flags(), n, nr, m as i32 * 16 + 8);
            }
        }
        self.b.ins().store(flags(), need, vm, (OFF_STACK + OFF_LEN) as i32);

        let fp = self.b.ins().load(ptr, flags(), vm, (OFF_FRAMES + OFF_PTR) as i32);
        let foff = self.b.ins().imul_imm(flen, FRAME_SIZE as i64);
        let f = self.b.ins().iadd(fp, foff);
        let depth = self.b.ins().load(types::I32, flags(), vm, OFF_DEPTH as i32);
        let ns = self.b.ins().load(types::I32, flags(), vm, OFF_NS as i32);
        let v = self.b.ins().iconst(types::I32, proto as i64);
        self.b.ins().store(flags(), v, f, F_PROTO as i32);
        self.b.ins().store(flags(), zero, f, F_PC as i32);
        self.b.ins().store(flags(), slen, f, F_BASE as i32);
        let v = self.b.ins().iconst(types::I16, argc as i64);
        self.b.ins().store(flags(), v, f, F_ARGC as i32);
        self.b.ins().store(flags(), depth, f, F_DEPTH as i32);
        let v = self.b.ins().iconst(types::I8, 1);
        self.b.ins().store(flags(), v, f, F_COUNTED as i32);
        let v = self.b.ins().iconst(types::I16, dst as i64);
        self.b.ins().store(flags(), v, f, F_RET as i32);
        let v = self.b.ins().iconst(types::I32, 0);
        self.b.ins().store(flags(), v, f, F_POST as i32);
        self.b.ins().store(flags(), undef, f, F_THIS as i32);
        self.b.ins().store(flags(), zero, f, F_THIS as i32 + 8);
        let v = self.b.ins().iconst(types::I32, NONE as i64);
        self.b.ins().store(flags(), v, f, F_SELF_CLASS as i32);
        self.b.ins().store(flags(), ns, f, F_NS as i32);
        self.b.ins().store(flags(), zero, f, F_PENDING as i32);
        let flen1 = self.b.ins().iadd_imm(flen, 1);
        self.b.ins().store(flags(), flen1, vm, (OFF_FRAMES + OFF_LEN) as i32);

        let slot = self.b.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, ENV_SIZE, 3));
        let ce = self.b.ins().stack_addr(ptr, slot, 0);
        let globals = self.globals;
        let fi1 = self.b.ins().iadd_imm(self.fi, 1);
        let argcv = self.b.ins().iconst(ptr, argc as i64);
        for (off, v) in [(0, nr), (8, globals), (16, vm), (24, fi1), (32, argcv), (40, slen), (48, zero)] {
            self.b.ins().store(flags(), v, ce, off);
        }
        let inst = self.b.ins().call_indirect(self.sigs.native, code, &[ce]);
        let status = self.b.inst_results(inst)[0];
        let (ok, bad) = (self.b.create_block(), self.b.create_block());
        self.b.ins().brif(status, bad, &[], ok, &[]);
        self.switch(ok);
        self.reload();
        self.jump_next(i);
        self.switch(bad);
        let (pc, one) = (self.b.ins().iconst(types::I32, i as i64), self.b.ins().iconst(types::I32, 1));
        let r = self.call(self.sigs.failed, h_call_failed as usize, &[self.env, pc, one]).unwrap();
        self.go_on(i, r);

        self.switch(slow);
        self.call_op(i);
    }

    /// `돌려주자`: the frame of a call that only wants the value ends here
    /// (its registers and object released); anything else (a caught signal
    /// waiting, a caller that wants more than the value, a type to check
    /// closer) goes through `finish_call`.
    fn return_op(&mut self, src: Option<Reg>) {
        let slow = self.b.create_block();
        let (vm, ptr) = (self.vm, self.ptr);
        let flen = self.b.ins().load(ptr, flags(), vm, (OFF_FRAMES + OFF_LEN) as i32);
        let fp = self.b.ins().load(ptr, flags(), vm, (OFF_FRAMES + OFF_PTR) as i32);
        let top = self.b.ins().iadd_imm(flen, -1);
        let foff = self.b.ins().imul_imm(top, FRAME_SIZE as i64);
        let f = self.b.ins().iadd(fp, foff);
        let post = self.b.ins().load(types::I32, flags(), f, F_POST as i32);
        let pending = self.b.ins().load(ptr, flags(), f, F_PENDING as i32);
        let this = self.b.ins().load(types::I32, flags(), f, F_THIS as i32);
        let has_caller = self.b.ins().icmp_imm(IntCC::UnsignedGreaterThanOrEqual, flen, 2);
        let _ = this;
        // Post::Value (0) takes the value; falling off the end, Post::Discard
        // (1: a constructor, a setter) takes nothing.
        let post_ok = match src {
            Some(_) => self.b.ins().icmp_imm(IntCC::Equal, post, 0),
            None => self.b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, post, 1),
        };
        let pend_ok = self.b.ins().icmp_imm(IntCC::Equal, pending, 0);
        let ok = post_ok;
        let ok = self.b.ins().band(ok, pend_ok);
        let ok = self.b.ins().band(ok, has_caller);
        let fast = self.b.create_block();
        self.b.ins().brif(ok, fast, &[], slow, &[]);
        self.switch(fast);
        let (vt, vp) = match src {
            Some(src) => {
                let va = self.addr(At::Reg(src));
                match self.simple_type(self.proto.return_type) {
                    Some(tags) => self.check_tags(va, tags, slow),
                    None => {
                        self.b.ins().jump(slow, &[]);
                        let dead = self.b.create_block();
                        self.switch(dead);
                    }
                }
                let vt = self.b.ins().load(types::I64, flags(), va, 0);
                let vp = self.b.ins().load(types::I64, flags(), va, 8);
                let undef = self.b.ins().iconst(types::I64, TAG_UNDEF);
                self.b.ins().store(flags(), undef, va, 0);
                (vt, vp)
            }
            // 비어있음 fits every return type.
            None => (self.b.ins().iconst(types::I64, TAG_NULL), self.b.ins().iconst(types::I64, 0)),
        };
        // The registers go (as `truncate` drops them).
        for r in 0..self.proto.nregs {
            let a = self.addr(At::Reg(r));
            self.release(a);
        }
        // A method's object.
        let this = self.b.ins().iadd_imm(f, F_THIS as i64);
        self.release(this);
        let counted = self.b.ins().load(types::I8, flags(), f, F_COUNTED as i32);
        let counted = self.b.ins().uextend(types::I32, counted);
        let d = self.b.ins().load(types::I32, flags(), vm, OFF_DEPTH as i32);
        let d = self.b.ins().isub(d, counted);
        self.b.ins().store(flags(), d, vm, OFF_DEPTH as i32);
        let base = self.b.ins().load(ptr, flags(), f, F_BASE as i32);
        self.b.ins().store(flags(), base, vm, (OFF_STACK + OFF_LEN) as i32);
        self.b.ins().store(flags(), top, vm, (OFF_FRAMES + OFF_LEN) as i32);
        let ret = self.b.ins().load(types::I16, flags(), f, F_RET as i32);
        let ret = self.b.ins().uextend(ptr, ret);
        let caller = self.b.ins().iadd_imm(f, -(FRAME_SIZE as i64));
        let cbase = self.b.ins().load(ptr, flags(), caller, F_BASE as i32);
        let slot = self.b.ins().iadd(cbase, ret);
        let slot = self.b.ins().ishl_imm(slot, 4);
        let sp = self.b.ins().load(ptr, flags(), vm, (OFF_STACK + OFF_PTR) as i32);
        let dst = self.b.ins().iadd(sp, slot);
        let r = self.b.ins().iconst(types::I32, S_RETURNED as i64);
        if src.is_some() {
            self.store(dst, vt, vp);
        } else {
            // Falling off the end: 비어있음 for Post::Value, nothing for Discard.
            let (put, done) = (self.b.create_block(), self.b.create_block());
            self.b.ins().brif(post, done, &[], put, &[]);
            self.switch(put);
            self.store(dst, vt, vp);
            self.b.ins().jump(done, &[]);
            self.switch(done);
        }
        self.b.ins().return_(&[r]);

        self.switch(slow);
        let r = match src {
            Some(src) => {
                let s = self.b.ins().iconst(types::I32, src as i64);
                self.call(self.sigs.ret, h_return as usize, &[self.env, s]).unwrap()
            }
            None => self.call(self.sigs.env_only, h_return_null as usize, &[self.env]).unwrap(),
        };
        self.b.ins().return_(&[r]);
    }

    /// Releases the value at `a` if it holds a reference.
    fn release(&mut self, a: V) {
        let t = self.tag_of(a);
        let t = self.b.ins().iadd_imm(t, -(tag::STR as i64));
        let counted = self.b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, t, (tag::RESOURCE - tag::STR) as i64);
        let (rel, cont) = (self.b.create_block(), self.b.create_block());
        self.b.ins().brif(counted, rel, &[], cont, &[]);
        self.switch(rel);
        if self.inline_rc {
            // Not the last reference: one fewer. The last one: Rust drops it.
            let p = self.payload_of(a);
            let n = self.b.ins().load(types::I64, flags(), p, RC_STRONG);
            let last = self.b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, n, 1);
            let (drop_it, fewer) = (self.b.create_block(), self.b.create_block());
            self.b.ins().brif(last, drop_it, &[], fewer, &[]);
            self.switch(fewer);
            let n1 = self.b.ins().iadd_imm(n, -1);
            self.b.ins().store(flags(), n1, p, RC_STRONG);
            self.b.ins().jump(cont, &[]);
            self.switch(drop_it);
        }
        self.call(self.sigs.one_ptr, h_drop as usize, &[a]);
        self.b.ins().jump(cont, &[]);
        self.switch(cont);
    }

    /// One more reference to the heap value at `a` (which has a counted tag).
    fn retain_counted(&mut self, a: V) {
        let p = self.payload_of(a);
        let n = self.b.ins().load(types::I64, flags(), p, RC_STRONG);
        let n1 = self.b.ins().iadd_imm(n, 1);
        self.b.ins().store(flags(), n1, p, RC_STRONG);
    }

    /// Instruction `i` through `h_op`, else the interpreter.
    fn direct(&mut self, i: usize) {
        let pc = self.b.ins().iconst(types::I32, i as i64);
        let r = self.call(self.sigs.step, h_op as usize, &[self.env, pc]).unwrap();
        let slow = self.slow_block(i);
        let (done, next) = (self.b.create_block(), self.next(i));
        let is_slow = self.b.ins().icmp_imm(IntCC::Equal, r, SLOW as i64);
        self.b.ins().brif(is_slow, slow, &[], done, &[]);
        self.switch(done);
        let is_next = self.b.ins().icmp_imm(IntCC::Equal, r, i as i64 + 1);
        self.b.ins().brif(is_next, next, &[], self.dispatch, &[BlockArg::Value(r)]);
    }

    /// A block that runs `i` through `direct`.
    fn direct_block(&mut self, i: usize) -> Block {
        let cur = self.b.current_block().unwrap();
        let blk = self.b.create_block();
        self.switch(blk);
        self.direct(i);
        self.switch(cur);
        blk
    }

    /// A call: straight into the callee's native code when it has some.
    fn call_op(&mut self, i: usize) {
        self.call_via(i, h_call_start as usize, h_call as usize)
    }

    /// A call started by `start` (`h_call_start`'s answers), else by `none`
    /// (a helper that does the whole call as the interpreter does).
    fn call_via(&mut self, i: usize, start: usize, none_fn: usize) {
        let slot = self.b.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, ENV_SIZE, 3));
        let callee_env = self.b.ins().stack_addr(self.ptr, slot, 0);
        let pc = self.b.ins().iconst(types::I32, i as i64);
        let code = self.call(self.sigs.start, start, &[self.env, pc, callee_env]).unwrap();
        let (none, failed, run) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
        let not_run = self.b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, code, 1);
        let other = self.b.create_block();
        self.b.ins().brif(not_run, other, &[], run, &[]);
        self.switch(other);
        self.b.ins().brif(code, failed, &[], none, &[]);

        // No native code: the interpreter's way.
        self.switch(none);
        let pc = self.b.ins().iconst(types::I32, i as i64);
        let r = self.call(self.sigs.step, none_fn, &[self.env, pc]).unwrap();
        self.go_on(i, r);

        // It did not start.
        self.switch(failed);
        let (pc, zero) = (self.b.ins().iconst(types::I32, i as i64), self.b.ins().iconst(types::I32, 0));
        let r = self.call(self.sigs.failed, h_call_failed as usize, &[self.env, pc, zero]).unwrap();
        self.go_on(i, r);

        self.switch(run);
        let inst = self.b.ins().call_indirect(self.sigs.native, code, &[callee_env]);
        let status = self.b.inst_results(inst)[0];
        let (ok, bad) = (self.b.create_block(), self.b.create_block());
        self.b.ins().brif(status, bad, &[], ok, &[]);
        self.switch(ok);
        self.reload();
        self.jump_next(i);
        self.switch(bad);
        let (pc, one) = (self.b.ins().iconst(types::I32, i as i64), self.b.ins().iconst(types::I32, 1));
        let r = self.call(self.sigs.failed, h_call_failed as usize, &[self.env, pc, one]).unwrap();
        self.go_on(i, r);
    }

    fn jump_next(&mut self, i: usize) {
        let next = self.next(i);
        self.b.ins().jump(next, &[]);
    }

    /// Goes on in a new block when both values are numbers (their values),
    /// else to `slow`.
    fn both_nums(&mut self, xa: V, ya: V, slow: Block) -> (V, V) {
        let (tx, ty) = (self.tag_of(xa), self.tag_of(ya));
        let (nx, ny) = (self.is_tag(tx, TAG_NUM), self.is_tag(ty, TAG_NUM));
        let both = self.b.ins().band(nx, ny);
        let fast = self.b.create_block();
        self.b.ins().brif(both, fast, &[], slow, &[]);
        self.switch(fast);
        (self.num_of(xa), self.num_of(ya))
    }

    /// The left operand, and the right one from a register or a constant.
    fn operands(&mut self, a: Reg, b: Option<Reg>, k: Option<f64>, slow: Block) -> (V, V) {
        let xa = self.addr(At::Reg(a));
        match (b, k) {
            (Some(b), _) => {
                let ya = self.addr(At::Reg(b));
                self.both_nums(xa, ya, slow)
            }
            (None, Some(y)) => {
                let t = self.tag_of(xa);
                let n = self.is_tag(t, TAG_NUM);
                let fast = self.b.create_block();
                self.b.ins().brif(n, fast, &[], slow, &[]);
                self.switch(fast);
                let x = self.num_of(xa);
                (x, self.b.ins().f64const(y))
            }
            _ => unreachable!(),
        }
    }

    fn binary(&mut self, i: usize, op: BinOp, dst: Reg, a: Reg, b: Option<Reg>, k: Option<f64>) {
        let slow = self.slow_block(i);
        let (x, y) = self.operands(a, b, k, slow);
        let d = self.addr(At::Reg(dst));
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => {
                let r = match op {
                    BinOp::Add => self.b.ins().fadd(x, y),
                    BinOp::Sub => self.b.ins().fsub(x, y),
                    BinOp::Mul => self.b.ins().fmul(x, y),
                    _ => {
                        // Dividing by zero is an error: the interpreter's.
                        let zero = self.b.ins().f64const(0.0);
                        let is_zero = self.b.ins().fcmp(FloatCC::Equal, y, zero);
                        let fast = self.b.create_block();
                        self.b.ins().brif(is_zero, slow, &[], fast, &[]);
                        self.switch(fast);
                        self.b.ins().fdiv(x, y)
                    }
                };
                let r = self.boxed(r);
                self.store_num(d, r);
            }
            BinOp::Mod => {
                // Go's int64 conversions, when both fit (else the interpreter).
                let lo = self.b.ins().f64const(-9_223_372_036_854_775_808.0);
                let hi = self.b.ins().f64const(9_223_372_036_854_775_808.0);
                let fits = |g: &mut Self, v: V| {
                    let a = g.b.ins().fcmp(FloatCC::GreaterThanOrEqual, v, lo);
                    let b = g.b.ins().fcmp(FloatCC::LessThan, v, hi);
                    g.b.ins().band(a, b)
                };
                let (fx, fy) = (fits(self, x), fits(self, y));
                let both = self.b.ins().band(fx, fy);
                let conv = self.b.create_block();
                self.b.ins().brif(both, conv, &[], slow, &[]);
                self.switch(conv);
                let xi = self.b.ins().fcvt_to_sint_sat(types::I64, x);
                let yi = self.b.ins().fcvt_to_sint_sat(types::I64, y);
                // 0 fails and -1 may overflow: the interpreter's.
                let y1 = self.b.ins().iadd_imm(yi, 1);
                let odd = self.b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, y1, 1);
                let fast = self.b.create_block();
                self.b.ins().brif(odd, slow, &[], fast, &[]);
                self.switch(fast);
                let r = self.b.ins().srem(xi, yi);
                let r = self.b.ins().fcvt_from_sint(types::F64, r);
                self.store_num(d, r);
            }
            _ => {
                let c = self.b.ins().fcmp(float_cc(op), x, y);
                self.store_bool(d, c);
            }
        }
        self.jump_next(i);
    }

    fn cmp_jump(&mut self, i: usize, op: BinOp, a: Reg, b: Option<Reg>, k: Option<f64>, to: u32) {
        let slow = self.slow_block(i);
        let (x, y) = self.operands(a, b, k, slow);
        let c = self.b.ins().fcmp(float_cc(op), x, y);
        let (to, next) = (self.blocks[to as usize], self.next(i));
        self.b.ins().brif(c, next, &[], to, &[]);
    }

    /// `dst = src.clone()`; `checked`: an undefined `src` is the
    /// interpreter's (a missing variable).
    fn copy(&mut self, i: usize, dst: At, src: At, checked: bool) {
        let s = self.addr(src);
        let t = self.tag_of(s);
        let heap_or_undef = if checked {
            // counted tags and UNDEF: t - STR, unsigned, above the counted range
            // is not enough (CLASS sits between), so test the two separately.
            let u = self.is_tag(t, TAG_UNDEF);
            let slow = self.slow_block(i);
            let fast = self.b.create_block();
            self.b.ins().brif(u, slow, &[], fast, &[]);
            self.switch(fast);
            t
        } else {
            t
        };
        let off = self.b.ins().iadd_imm(heap_or_undef, -(tag::STR as i64));
        let counted = self.b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, off, (tag::RESOURCE - tag::STR) as i64);
        let shared = self.b.create_block();
        let plain = self.b.create_block();
        self.b.ins().brif(counted, shared, &[], plain, &[]);

        self.switch(shared);
        if self.inline_rc {
            self.retain_counted(s);
            let p = self.payload_of(s);
            let d = self.addr(dst);
            self.store(d, t, p);
        } else {
            let d = self.addr(dst);
            self.call(self.sigs.two_ptr, h_clone_into as usize, &[d, s]);
        }
        self.jump_next(i);

        self.switch(plain);
        let p = self.payload_of(s);
        let d = self.addr(dst);
        self.store(d, t, p);
        self.jump_next(i);
    }

    /// Copies the value at `s` to `d` (one more reference to a heap value).
    fn copy_value(&mut self, s: V, d: V) {
        let t = self.tag_of(s);
        let p = self.payload_of(s);
        let off = self.b.ins().iadd_imm(t, -(tag::STR as i64));
        let counted = self.b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, off, (tag::RESOURCE - tag::STR) as i64);
        let (shared, done) = (self.b.create_block(), self.b.create_block());
        self.b.ins().brif(counted, shared, &[], done, &[]);
        self.switch(shared);
        if self.inline_rc {
            self.retain_counted(s);
            self.b.ins().jump(done, &[]);
            self.switch(done);
            self.store(d, t, p);
        } else {
            let (after, plain) = (self.b.create_block(), done);
            self.call(self.sigs.two_ptr, h_clone_into as usize, &[d, s]);
            self.b.ins().jump(after, &[]);
            self.switch(plain);
            self.store(d, t, p);
            self.b.ins().jump(after, &[]);
            self.switch(after);
        }
    }

    /// `'나'`: the frame's object when it has one (else `h_op`).
    fn self_or(&mut self, i: usize, dst: Reg) {
        let fallback = self.direct_block(i);
        let fp = self.b.ins().load(self.ptr, flags(), self.vm, (OFF_FRAMES + OFF_PTR) as i32);
        let off = self.b.ins().imul_imm(self.fi, FRAME_SIZE as i64);
        let f = self.b.ins().iadd(fp, off);
        let this = self.b.ins().iadd_imm(f, F_THIS as i64);
        let t = self.tag_of(this);
        let none = self.is_tag(t, TAG_UNDEF);
        let go = self.b.create_block();
        self.b.ins().brif(none, fallback, &[], go, &[]);
        self.switch(go);
        let d = self.addr(At::Reg(dst));
        self.copy_value(this, d);
        self.jump_next(i);
    }

    /// The property that instruction `i`'s cache knows, of the object in
    /// register `obj`: its value's address, or `fallback`.
    fn cached_prop(&mut self, i: usize, obj: Reg, name: u32, fallback: Block) -> V {
        let ic = self.b.ins().iconst(self.ptr, unsafe { self.ics.add(i) } as i64);
        let oa = self.addr(At::Reg(obj));
        let t = self.tag_of(oa);
        let is_obj = self.is_tag(t, tag::OBJECT as i64);
        let a = self.b.create_block();
        self.b.ins().brif(is_obj, a, &[], fallback, &[]);
        self.switch(a);
        let p = self.payload_of(oa);
        let class = self.b.ins().load(types::I32, flags(), p, OFF_OBJ_CLASS as i32);
        let cached = self.b.ins().load(types::I32, MemFlags::new(), ic, 0);
        let same = self.b.ins().icmp(IntCC::Equal, class, cached);
        let b = self.b.create_block();
        self.b.ins().brif(same, b, &[], fallback, &[]);
        self.switch(b);
        let pos = self.b.ins().load(types::I32, MemFlags::new(), ic, 4);
        let pos = self.b.ins().uextend(types::I64, pos);
        let props = self.off_props as i32;
        let items = self.b.ins().load(self.ptr, flags(), p, props + OFF_PTR as i32);
        let len = self.b.ins().load(types::I64, flags(), p, props + OFF_LEN as i32);
        let inside = self.b.ins().icmp(IntCC::UnsignedLessThan, pos, len);
        let c = self.b.create_block();
        self.b.ins().brif(inside, c, &[], fallback, &[]);
        self.switch(c);
        let off = self.b.ins().imul_imm(pos, PROP_SIZE as i64);
        let e = self.b.ins().iadd(items, off);
        let n = self.b.ins().load(types::I32, flags(), e, PROP_NAME as i32);
        let hit = self.b.ins().icmp_imm(IntCC::Equal, n, name as i64);
        let d = self.b.create_block();
        self.b.ins().brif(hit, d, &[], fallback, &[]);
        self.switch(d);
        self.b.ins().iadd_imm(e, PROP_VALUE as i64)
    }

    /// A block for when the cache does not know the value in `obj`: a
    /// dictionary goes on to the key (`'표'의 "가"`, the next instruction),
    /// anything else to `fallback`.
    fn dict_goes_on(&mut self, i: usize, obj: Reg, fallback: Block) -> Block {
        let cur = self.b.current_block().unwrap();
        let blk = self.b.create_block();
        self.switch(blk);
        let oa = self.addr(At::Reg(obj));
        let t = self.tag_of(oa);
        let dict = self.is_tag(t, tag::DICT as i64);
        let next = self.next(i);
        self.b.ins().brif(dict, next, &[], fallback, &[]);
        self.switch(cur);
        blk
    }

    /// A property read (`'점'의 '가로'`): from where the cache says it is.
    fn member(&mut self, i: usize, dst: Reg, obj: Reg, name: u32, skip: u32) {
        let fallback = self.direct_block(i);
        let fallback = self.dict_goes_on(i, obj, fallback);
        let v = self.cached_prop(i, obj, name, fallback);
        self.check_ic_access(i, obj, fallback);
        let d = self.addr(At::Reg(dst));
        self.copy_value(v, d);
        let skip = self.blocks[skip as usize];
        self.b.ins().jump(skip, &[]);
    }

    /// Goes on when the cache of instruction `i` says anyone may use the
    /// member of the object in `obj`, or (1) the running frame is a method,
    /// or (2) a method of that object; else to `fallback`.
    fn check_ic_access(&mut self, i: usize, obj: Reg, fallback: Block) {
        let ic = self.b.ins().iconst(self.ptr, unsafe { self.ics.add(i) } as i64);
        let access = self.b.ins().load(types::I32, MemFlags::new(), ic, 12);
        let (checks, read) = (self.b.create_block(), self.b.create_block());
        self.b.ins().brif(access, checks, &[], read, &[]);
        self.switch(checks);
        let fp = self.b.ins().load(self.ptr, flags(), self.vm, (OFF_FRAMES + OFF_PTR) as i32);
        let off = self.b.ins().imul_imm(self.fi, FRAME_SIZE as i64);
        let f = self.b.ins().iadd(fp, off);
        let this_tag = self.b.ins().load(types::I32, flags(), f, F_THIS as i32);
        let has_this = self.b.ins().icmp_imm(IntCC::NotEqual, this_tag, TAG_UNDEF);
        let this_p = self.b.ins().load(types::I64, flags(), f, F_THIS as i32 + 8);
        let oa = self.addr(At::Reg(obj));
        let obj_p = self.payload_of(oa);
        let same = self.b.ins().icmp(IntCC::Equal, this_p, obj_p);
        let protected = self.b.ins().icmp_imm(IntCC::Equal, access, 1);
        let ok = self.b.ins().bor(protected, same);
        let ok = self.b.ins().band(ok, has_this);
        self.b.ins().brif(ok, read, &[], fallback, &[]);
        self.switch(read);
    }

    /// Before a method call: an object of the class the cache knows, whose
    /// method the running code may call (else `h_op` checks it all).
    fn method_prep(&mut self, i: usize, obj: Reg) {
        let fallback = self.direct_block(i);
        let ic = self.b.ins().iconst(self.ptr, unsafe { self.ics.add(i) } as i64);
        let oa = self.addr(At::Reg(obj));
        let t = self.tag_of(oa);
        let is_obj = self.is_tag(t, tag::OBJECT as i64);
        let a = self.b.create_block();
        self.b.ins().brif(is_obj, a, &[], fallback, &[]);
        self.switch(a);
        let p = self.payload_of(oa);
        let class = self.b.ins().load(types::I32, flags(), p, OFF_OBJ_CLASS as i32);
        let cached = self.b.ins().load(types::I32, MemFlags::new(), ic, 0);
        let same = self.b.ins().icmp(IntCC::Equal, class, cached);
        let b = self.b.create_block();
        self.b.ins().brif(same, b, &[], fallback, &[]);
        self.switch(b);
        self.check_ic_access(i, obj, fallback);
        self.jump_next(i);
    }

    /// A variable whose first place is register `r`: its value when it has
    /// one (else `h_op` looks further).
    fn get_var_reg(&mut self, i: usize, dst: Reg, r: Reg) {
        let fallback = self.direct_block(i);
        let s = self.addr(At::Reg(r));
        let t = self.tag_of(s);
        let undef = self.is_tag(t, TAG_UNDEF);
        let go = self.b.create_block();
        self.b.ins().brif(undef, fallback, &[], go, &[]);
        self.switch(go);
        let d = self.addr(At::Reg(dst));
        self.copy_value(s, d);
        self.jump_next(i);
    }

    /// A property write: to where the cache says it is, when the value is
    /// what the field's type takes.
    fn set_member(&mut self, i: usize, obj: Reg, val: Reg, name: u32, skip: u32) {
        let fallback = self.direct_block(i);
        let fallback = self.dict_goes_on(i, obj, fallback);
        let v = self.cached_prop(i, obj, name, fallback);
        let ic = self.b.ins().iconst(self.ptr, unsafe { self.ics.add(i) } as i64);
        let check = self.b.ins().load(types::I32, MemFlags::new(), ic, 8);
        let va = self.addr(At::Reg(val));
        let t = self.tag_of(va);
        // Which tag the check wants (none for 0), or 비어있음.
        let want = {
            let (n, s_, b_) = (
                self.b.ins().iconst(types::I32, TAG_NUM),
                self.b.ins().iconst(types::I32, tag::STR as i64),
                self.b.ins().iconst(types::I32, TAG_BOOL),
            );
            let is1 = self.b.ins().icmp_imm(IntCC::Equal, check, 1);
            let is2 = self.b.ins().icmp_imm(IntCC::Equal, check, 2);
            let x = self.b.ins().select(is2, s_, b_);
            self.b.ins().select(is1, n, x)
        };
        let any = self.b.ins().icmp_imm(IntCC::Equal, check, 0);
        let fits = self.b.ins().icmp(IntCC::Equal, t, want);
        let null = self.is_tag(t, TAG_NULL);
        let ok = self.b.ins().bor(any, fits);
        let ok = self.b.ins().bor(ok, null);
        let go = self.b.create_block();
        self.b.ins().brif(ok, go, &[], fallback, &[]);
        self.switch(go);
        self.copy_value(va, v);
        let skip = self.blocks[skip as usize];
        self.b.ins().jump(skip, &[]);
    }

    /// `dst = take(src)` (the register is left undefined).
    fn take(&mut self, i: usize, dst: At, src: Reg) {
        if let At::Reg(d) = dst {
            if d == src {
                return self.jump_next(i);
            }
        }
        let s = self.addr(At::Reg(src));
        let t = self.tag_of(s);
        let p = self.payload_of(s);
        let u = self.b.ins().iconst(types::I64, TAG_UNDEF);
        self.b.ins().store(flags(), u, s, 0);
        let d = self.addr(dst);
        self.store(d, t, p);
        self.jump_next(i);
    }
}

/// The type ids a number fits without a closer look (0 is no type).
fn number_types(prog: &Program) -> impl Iterator<Item = u32> + '_ {
    std::iter::once(0).chain(
        prog.types
            .iter()
            .enumerate()
            .skip(1)
            .filter(|(_, t)| matches!(t.kind, TypeKind::Number | TypeKind::Any))
            .map(|(i, _)| i as u32)
            .take(8),
    )
}

/// Whether compiled code does `op` itself (in the common case) rather than
/// hand it to the interpreter.
fn native(op: &Op, prog: &Program) -> bool {
    match *op {
        Op::LoadK { .. } => true,
        Op::BinK { k, .. } | Op::CmpKJump { k, .. } => prog.consts[k as usize].as_num().is_some(),
        Op::Update { .. } => true,
        Op::LoadNull { .. }
        | Op::LoadBool { .. }
        | Op::Move { .. }
        | Op::GetReg { .. }
        | Op::GetGlobal { .. }
        | Op::SetReg { .. }
        | Op::SetGlobal { .. }
        | Op::Bin { .. }
        | Op::Undef { .. }
        | Op::Eq { .. }
        | Op::Truth { .. }
        | Op::Jump { .. }
        | Op::JumpIfFalse { .. }
        | Op::JumpIfTrue { .. }
        | Op::CmpJump { .. }
        | Op::RangePrep { .. }
        | Op::RangeTest { .. }
        | Op::RangeStep { .. }
        | Op::Boxed { .. }
        | Op::Enter
        | Op::Leave
        | Op::Call { .. }
        | Op::ArgGiven { .. }
        | Op::Return { .. }
        | Op::ReturnNull
        | Op::CallMethod { .. }
        | Op::GetVar { .. }
        | Op::Decl { .. }
        | Op::Assign { .. }
        | Op::SelfOr { .. }
        | Op::GetThis { .. }
        | Op::Member { .. }
        | Op::Index { .. }
        | Op::IndexK { .. }
        | Op::SetIndex { .. }
        | Op::SetIndexK { .. }
        | Op::SetMember { .. }
        | Op::MethodPrep { .. }
        | Op::Format { .. }
        | Op::Concat { .. }
        | Op::IterNext { .. }
        | Op::ListCheck { .. }
        | Op::ListPush { .. }
        | Op::NewObj { .. }
        | Op::InitField { .. }
        | Op::CallCtor { .. } => true,
        _ => false,
    }
}

fn float_cc(op: BinOp) -> FloatCC {
    match op {
        BinOp::Gt => FloatCC::GreaterThan,
        BinOp::Lt => FloatCC::LessThan,
        BinOp::Ge => FloatCC::GreaterThanOrEqual,
        _ => FloatCC::LessThanOrEqual,
    }
}
