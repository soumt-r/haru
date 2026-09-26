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

pub(super) struct Jit {
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
        let mut flags = settings::builder();
        // Quicker compiles: the code mostly moves values in memory, which the
        // optimizer gains nothing on. (The single-pass register allocator
        // compiles faster still, but its code runs twice as long.)
        flags.set("opt_level", "none").ok()?;
        flags.set("enable_verifier", "false").ok()?;
        flags.set("use_colocated_libcalls", "false").ok()?;
        flags.set("is_pic", "false").ok()?;
        let isa = cranelift_native::builder().ok()?.finish(settings::Flags::new(flags)).ok()?;
        let module = JITModule::new(JITBuilder::with_isa(isa, cranelift_module::default_libcall_names()));
        let ctx = module.make_context();
        // Compiled code writes `Post::Value` as 0.
        if unsafe { *(&Post::Value as *const Post as *const u32) } != 0 {
            return None;
        }
        Some(Jit {
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
                let n = &mut self.counts[id as usize];
                *n += 1;
                if *n <= self.threshold {
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
                let c = self.compile(&prog.protos[id as usize], prog);
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

    fn compile(&mut self, proto: &Proto, prog: &Program) -> Option<Code> {
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
        {
            let b = FunctionBuilder::new(&mut self.ctx.func, &mut self.fctx);
            Gen::new(b, ptr, cc, proto, prog, self.table.as_ptr()).build();
        }
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
        if let Err(e) = self.module.define_function(id, &mut self.ctx) {
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

// ---- helpers the compiled code calls

unsafe extern "C" fn h_step(env: *mut Env, pc: u32) -> u32 {
    let env = &mut *env;
    let vm = &mut *env.vm;
    let fi = env.fi;
    vm.frames[fi].pc = pc as usize;
    let r = vm.exec_mode::<true>();
    let next = vm.jit_resume(fi, r);
    env.refresh();
    next
}

unsafe extern "C" fn h_call(env: *mut Env, pc: u32) -> u32 {
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

unsafe extern "C" fn h_return(env: *mut Env, src: u32) -> u32 {
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
    std::ptr::drop_in_place(v);
}

unsafe extern "C" fn h_clone_into(dst: *mut Value, src: *const Value) {
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
    regs: Variable,
    globals: Variable,
    blocks: Vec<Block>,
    dispatch: Block,
    trap: Block,
    sigs: Sigs,
}

/// Where a value sits: a register or a global.
#[derive(Clone, Copy)]
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
        let globals = b.declare_var(ptr);
        let g0 = b.ins().load(ptr, flags(), env, 8);
        b.def_var(globals, g0);
        let mut g = Gen { b, ptr, proto, prog, env, vm, fi, base_off, table, regs, globals, blocks, dispatch, trap, sigs };
        g.reload();
        let start = g.b.ins().load(types::I64, flags(), g.env, 48);
        let at = g.b.ins().ireduce(types::I32, start);
        let (first, dispatch) = (g.blocks[0], g.dispatch);
        g.b.ins().brif(start, dispatch, &[BlockArg::Value(at)], first, &[]);
        g
    }

    fn build(mut self) {
        for i in 0..self.proto.code.len() {
            let blk = self.blocks[i];
            self.b.switch_to_block(blk);
            self.op(i);
        }
        self.emit_dispatch();
        self.b.switch_to_block(self.trap);
        self.b.ins().trap(TrapCode::unwrap_user(1));
        self.b.seal_all_blocks();
        self.b.finalize();
    }

    /// The registers' and globals' addresses, again (after a helper that may
    /// have moved the stack).
    fn reload(&mut self) {
        let sp = self.b.ins().load(self.ptr, flags(), self.vm, (OFF_STACK + OFF_PTR) as i32);
        let r = self.b.ins().iadd(sp, self.base_off);
        self.b.def_var(self.regs, r);
    }

    fn addr(&mut self, at: At) -> V {
        let (var, i) = match at {
            At::Reg(r) => (self.regs, r as i64),
            At::Global(g) => (self.globals, g as i64),
        };
        let base = self.b.use_var(var);
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
        let old = self.tag_of(a);
        let t = self.b.ins().iadd_imm(old, -(tag::STR as i64));
        let counted = self.b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, t, (tag::RESOURCE - tag::STR) as i64);
        let release = self.b.create_block();
        let cont = self.b.create_block();
        self.b.ins().brif(counted, release, &[], cont, &[]);
        self.b.switch_to_block(release);
        self.call(self.sigs.one_ptr, h_drop as usize, &[a]);
        self.b.ins().jump(cont, &[]);
        self.b.switch_to_block(cont);
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
        self.b.switch_to_block(slow);
        self.step(i);
        self.b.switch_to_block(cur);
        slow
    }

    fn emit_dispatch(&mut self) {
        self.b.switch_to_block(self.dispatch);
        let r = self.b.block_params(self.dispatch)[0];
        let special = self.b.ins().icmp_imm(IntCC::UnsignedGreaterThanOrEqual, r, RETURNED as i64);
        let leave = self.b.create_block();
        let table = self.b.create_block();
        self.b.ins().brif(special, leave, &[], table, &[]);

        self.b.switch_to_block(leave);
        let status = self.b.ins().iadd_imm(r, -(RETURNED as i64));
        self.b.ins().return_(&[status]);

        self.b.switch_to_block(table);
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
                    _ => return self.step(i),
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
            Op::Update { var, a, b, op } if matches!(op, BinOp::Add | BinOp::Sub) => {
                let (target, meta) = match self.prog.vars[var as usize].slots.as_slice() {
                    [Slot { loc: Loc::Reg(r), meta }] => (At::Reg(*r), *meta),
                    [Slot { loc: Loc::Global(g), meta }] => (At::Global(*g), *meta),
                    _ => return self.step(i),
                };
                let slow = self.slow_block(i);
                if let Some(m) = meta {
                    // A declared type or 고정: a number fits when the type is
                    // none, a number or anything, and it is not a constant.
                    let m = match m {
                        Loc::Reg(r) => At::Reg(r),
                        Loc::Global(g) => At::Global(g),
                        Loc::This(_) => return self.step(i),
                    };
                    let ma = self.addr(m);
                    let t = self.tag_of(ma);
                    let is_num = self.is_tag(t, TAG_NUM);
                    let (typed, untyped) = (self.b.create_block(), self.b.create_block());
                    let not_num = self.b.create_block();
                    self.b.ins().brif(is_num, typed, &[], not_num, &[]);
                    self.b.switch_to_block(not_num);
                    // Undefined or null: no type.
                    let u = self.is_tag(t, TAG_UNDEF);
                    let n = self.is_tag(t, TAG_NULL);
                    let none = self.b.ins().bor(u, n);
                    self.b.ins().brif(none, untyped, &[], slow, &[]);
                    self.b.switch_to_block(typed);
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
                    self.b.switch_to_block(untyped);
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
                let slow = self.slow_block(i);
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
                self.b.switch_to_block(fast);
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
                self.b.switch_to_block(fast);
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
                self.b.switch_to_block(fast);
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
                self.b.switch_to_block(fast);
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
            Op::ArgGiven { index, skip } => {
                let argc = self.b.ins().load(types::I64, flags(), self.env, 32);
                let given = self.b.ins().icmp_imm(IntCC::UnsignedGreaterThan, argc, index as i64);
                let (skip, next) = (self.blocks[skip as usize], self.next(i));
                self.b.ins().brif(given, skip, &[], next, &[]);
            }
            Op::Return { src } => self.return_op(src),
            Op::ReturnNull => {
                let r = self.call(self.sigs.env_only, h_return_null as usize, &[self.env]).unwrap();
                self.b.ins().return_(&[r]);
            }
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
        self.b.switch_to_block(fast);
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
        self.b.switch_to_block(fast);
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
        self.b.switch_to_block(go);
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
        let globals = self.b.use_var(self.globals);
        let fi1 = self.b.ins().iadd_imm(self.fi, 1);
        let argcv = self.b.ins().iconst(ptr, argc as i64);
        for (off, v) in [(0, nr), (8, globals), (16, vm), (24, fi1), (32, argcv), (40, slen), (48, zero)] {
            self.b.ins().store(flags(), v, ce, off);
        }
        let inst = self.b.ins().call_indirect(self.sigs.native, code, &[ce]);
        let status = self.b.inst_results(inst)[0];
        let (ok, bad) = (self.b.create_block(), self.b.create_block());
        self.b.ins().brif(status, bad, &[], ok, &[]);
        self.b.switch_to_block(ok);
        self.reload();
        self.jump_next(i);
        self.b.switch_to_block(bad);
        let (pc, one) = (self.b.ins().iconst(types::I32, i as i64), self.b.ins().iconst(types::I32, 1));
        let r = self.call(self.sigs.failed, h_call_failed as usize, &[self.env, pc, one]).unwrap();
        self.go_on(i, r);

        self.b.switch_to_block(slow);
        self.call_op(i);
    }

    /// `돌려주자`: a plain call's frame ends here (the caller takes the
    /// value); anything else (a method's object, a caught signal waiting, a
    /// caller that wants more than the value, a type to check closer) goes
    /// through `finish_call`.
    fn return_op(&mut self, src: Reg) {
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
        let this_ok = self.b.ins().icmp_imm(IntCC::Equal, this, TAG_UNDEF);
        let post_ok = self.b.ins().icmp_imm(IntCC::Equal, post, 0);
        let pend_ok = self.b.ins().icmp_imm(IntCC::Equal, pending, 0);
        let ok = self.b.ins().band(this_ok, post_ok);
        let ok = self.b.ins().band(ok, pend_ok);
        let ok = self.b.ins().band(ok, has_caller);
        let fast = self.b.create_block();
        self.b.ins().brif(ok, fast, &[], slow, &[]);
        self.b.switch_to_block(fast);
        let va = self.addr(At::Reg(src));
        match self.simple_type(self.proto.return_type) {
            Some(tags) => self.check_tags(va, tags, slow),
            None => {
                self.b.ins().jump(slow, &[]);
                let dead = self.b.create_block();
                self.b.switch_to_block(dead);
            }
        }
        let vt = self.b.ins().load(types::I64, flags(), va, 0);
        let vp = self.b.ins().load(types::I64, flags(), va, 8);
        let undef = self.b.ins().iconst(types::I64, TAG_UNDEF);
        self.b.ins().store(flags(), undef, va, 0);
        // The registers go (as `truncate` drops them).
        for r in 0..self.proto.nregs {
            let a = self.addr(At::Reg(r));
            self.release(a);
        }
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
        self.store(dst, vt, vp);
        let r = self.b.ins().iconst(types::I32, S_RETURNED as i64);
        self.b.ins().return_(&[r]);

        self.b.switch_to_block(slow);
        let s = self.b.ins().iconst(types::I32, src as i64);
        let r = self.call(self.sigs.ret, h_return as usize, &[self.env, s]).unwrap();
        self.b.ins().return_(&[r]);
    }

    /// Releases the value at `a` if it holds a reference.
    fn release(&mut self, a: V) {
        let t = self.tag_of(a);
        let t = self.b.ins().iadd_imm(t, -(tag::STR as i64));
        let counted = self.b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, t, (tag::RESOURCE - tag::STR) as i64);
        let (rel, cont) = (self.b.create_block(), self.b.create_block());
        self.b.ins().brif(counted, rel, &[], cont, &[]);
        self.b.switch_to_block(rel);
        self.call(self.sigs.one_ptr, h_drop as usize, &[a]);
        self.b.ins().jump(cont, &[]);
        self.b.switch_to_block(cont);
    }

    /// A call: straight into the callee's native code when it has some.
    fn call_op(&mut self, i: usize) {
        let slot = self.b.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, ENV_SIZE, 3));
        let callee_env = self.b.ins().stack_addr(self.ptr, slot, 0);
        let pc = self.b.ins().iconst(types::I32, i as i64);
        let code = self.call(self.sigs.start, h_call_start as usize, &[self.env, pc, callee_env]).unwrap();
        let (none, failed, run) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
        let not_run = self.b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, code, 1);
        let other = self.b.create_block();
        self.b.ins().brif(not_run, other, &[], run, &[]);
        self.b.switch_to_block(other);
        self.b.ins().brif(code, failed, &[], none, &[]);

        // No native code: the interpreter's way.
        self.b.switch_to_block(none);
        let pc = self.b.ins().iconst(types::I32, i as i64);
        let r = self.call(self.sigs.step, h_call as usize, &[self.env, pc]).unwrap();
        self.go_on(i, r);

        // It did not start.
        self.b.switch_to_block(failed);
        let (pc, zero) = (self.b.ins().iconst(types::I32, i as i64), self.b.ins().iconst(types::I32, 0));
        let r = self.call(self.sigs.failed, h_call_failed as usize, &[self.env, pc, zero]).unwrap();
        self.go_on(i, r);

        self.b.switch_to_block(run);
        let inst = self.b.ins().call_indirect(self.sigs.native, code, &[callee_env]);
        let status = self.b.inst_results(inst)[0];
        let (ok, bad) = (self.b.create_block(), self.b.create_block());
        self.b.ins().brif(status, bad, &[], ok, &[]);
        self.b.switch_to_block(ok);
        self.reload();
        self.jump_next(i);
        self.b.switch_to_block(bad);
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
        self.b.switch_to_block(fast);
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
                self.b.switch_to_block(fast);
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
                        self.b.switch_to_block(fast);
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
                self.b.switch_to_block(conv);
                let xi = self.b.ins().fcvt_to_sint_sat(types::I64, x);
                let yi = self.b.ins().fcvt_to_sint_sat(types::I64, y);
                // 0 fails and -1 may overflow: the interpreter's.
                let y1 = self.b.ins().iadd_imm(yi, 1);
                let odd = self.b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, y1, 1);
                let fast = self.b.create_block();
                self.b.ins().brif(odd, slow, &[], fast, &[]);
                self.b.switch_to_block(fast);
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
            self.b.switch_to_block(fast);
            t
        } else {
            t
        };
        let off = self.b.ins().iadd_imm(heap_or_undef, -(tag::STR as i64));
        let counted = self.b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, off, (tag::RESOURCE - tag::STR) as i64);
        let shared = self.b.create_block();
        let plain = self.b.create_block();
        self.b.ins().brif(counted, shared, &[], plain, &[]);

        self.b.switch_to_block(shared);
        let d = self.addr(dst);
        self.call(self.sigs.two_ptr, h_clone_into as usize, &[d, s]);
        self.jump_next(i);

        self.b.switch_to_block(plain);
        let p = self.payload_of(s);
        let d = self.addr(dst);
        self.store(d, t, p);
        self.jump_next(i);
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
        Op::LoadK { k, .. } => matches!(prog.consts[k as usize].tag(), tag::NUM | tag::BOOL | tag::NULL),
        Op::BinK { k, .. } | Op::CmpKJump { k, .. } => prog.consts[k as usize].as_num().is_some(),
        Op::Update { var, op, .. } => {
            matches!(op, BinOp::Add | BinOp::Sub)
                && matches!(
                    prog.vars[var as usize].slots.as_slice(),
                    [Slot { loc: Loc::Reg(_) | Loc::Global(_), meta: None | Some(Loc::Reg(_) | Loc::Global(_)) }]
                )
        }
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
        | Op::ReturnNull => true,
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
