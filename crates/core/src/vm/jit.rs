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
use cranelift_codegen::ir::{Block, SigRef, Value as V};
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
}

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

/// Runs the compiled code of frame `fi` (the running frame, at its start).
pub(super) fn invoke(vm: &mut Vm, code: Code, fi: usize) -> Done {
    let base = vm.frames[fi].base;
    let mut env = Env {
        regs: unsafe { vm.stack.as_mut_ptr().add(base) },
        globals: vm.globals.as_mut_ptr(),
        vm: vm as *mut Vm as *mut Vm<'static>,
        fi,
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
    module: JITModule,
    ctx: Context,
    fctx: FunctionBuilderContext,
    states: Vec<State>,
}

impl Jit {
    pub(super) fn new(protos: usize) -> Option<Jit> {
        let mut flags = settings::builder();
        flags.set("opt_level", "speed").ok()?;
        flags.set("use_colocated_libcalls", "false").ok()?;
        flags.set("is_pic", "false").ok()?;
        let isa = cranelift_native::builder().ok()?.finish(settings::Flags::new(flags)).ok()?;
        let module = JITModule::new(JITBuilder::with_isa(isa, cranelift_module::default_libcall_names()));
        let ctx = module.make_context();
        Some(Jit { module, ctx, fctx: FunctionBuilderContext::new(), states: (0..protos).map(|_| State::Untried).collect() })
    }

    fn code(&mut self, id: u32, prog: &Program) -> Option<Code> {
        match self.states[id as usize] {
            State::Ready(c) => Some(c),
            State::Failed => None,
            State::Untried => {
                let c = self.compile(&prog.protos[id as usize], prog);
                self.states[id as usize] = match c {
                    Some(c) => State::Ready(c),
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
            Gen::new(b, ptr, cc, proto, prog).build();
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
        let saved = self.stop_at;
        self.stop_at = fi + 1;
        let proto = self.frames[fi + 1].proto;
        let mut r = match self.jit_code(proto) {
            Some(code) => match invoke(self, code, fi + 1) {
                Done::Returned | Done::End => Ok(()),
                Done::Failed(s) => Err(s),
            },
            None => self.exec(),
        };
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

unsafe extern "C" fn h_return(env: *mut Env, src: u32) -> u32 {
    let env = &mut *env;
    let vm = &mut *env.vm;
    let v = mem::replace(&mut *env.regs.add(src as usize), Value::UNDEF);
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

/// `Op::Enter` when it cannot fail (else 1: the interpreter reports it).
unsafe extern "C" fn h_enter(env: *mut Env) -> u32 {
    let vm = &mut *(*env).vm;
    if vm.depth >= MAX_CALL_DEPTH {
        return 1;
    }
    vm.depth += 1;
    0
}

unsafe extern "C" fn h_leave(env: *mut Env) {
    let vm = &mut *(*env).vm;
    vm.depth -= 1;
}

unsafe extern "C" fn h_drop(v: *mut Value) {
    std::ptr::drop_in_place(v);
}

unsafe extern "C" fn h_clone_into(dst: *mut Value, src: *const Value) {
    *dst = (*src).clone();
}

// ---- code generation

const TAG_NULL: i64 = tag::NULL as i64;
const TAG_BOOL: i64 = tag::BOOL as i64;
const TAG_NUM: i64 = tag::NUM as i64;
const TAG_UNDEF: i64 = crate::value::UNDEF as i64;

struct Sigs {
    step: SigRef,
    ret: SigRef,
    env_only: SigRef,
    env_void: SigRef,
    one_ptr: SigRef,
    two_ptr: SigRef,
}

struct Gen<'a, 'b> {
    b: FunctionBuilder<'b>,
    ptr: types::Type,
    proto: &'a Proto,
    prog: &'a Program,
    env: V,
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
    fn new(mut b: FunctionBuilder<'b>, ptr: types::Type, cc: cranelift_codegen::isa::CallConv, proto: &'a Proto, prog: &'a Program) -> Self {
        let mut sig = |params: &[types::Type], ret: Option<types::Type>| {
            let mut s = Signature::new(cc);
            s.params.extend(params.iter().map(|t| AbiParam::new(*t)));
            s.returns.extend(ret.map(AbiParam::new));
            b.import_signature(s)
        };
        let sigs = Sigs {
            step: sig(&[ptr, types::I32], Some(types::I32)),
            ret: sig(&[ptr, types::I32], Some(types::I32)),
            env_only: sig(&[ptr], Some(types::I32)),
            env_void: sig(&[ptr], None),
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
        let regs = b.declare_var(ptr);
        let globals = b.declare_var(ptr);
        let mut g = Gen { b, ptr, proto, prog, env, regs, globals, blocks, dispatch, trap, sigs };
        g.reload();
        let first = g.blocks[0];
        g.b.ins().jump(first, &[]);
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
        let r = self.b.ins().load(self.ptr, flags(), self.env, 0);
        self.b.def_var(self.regs, r);
        let g = self.b.ins().load(self.ptr, flags(), self.env, 8);
        self.b.def_var(self.globals, g);
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
                let target = match self.prog.vars[var as usize].slots.as_slice() {
                    [Slot { loc: Loc::Reg(r), meta: None }] => At::Reg(*r),
                    [Slot { loc: Loc::Global(g), meta: None }] => At::Global(*g),
                    _ => return self.step(i),
                };
                let slow = self.slow_block(i);
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
                let r = self.call(self.sigs.env_only, h_enter as usize, &[self.env]).unwrap();
                let next = self.next(i);
                self.b.ins().brif(r, slow, &[], next, &[]);
            }
            Op::Leave => {
                self.call(self.sigs.env_void, h_leave as usize, &[self.env]);
                self.jump_next(i);
            }
            Op::Call { .. } => {
                let pc = self.b.ins().iconst(types::I32, i as i64);
                let r = self.call(self.sigs.step, h_call as usize, &[self.env, pc]).unwrap();
                self.go_on(i, r);
            }
            Op::Return { src } => {
                let s = self.b.ins().iconst(types::I32, src as i64);
                let r = self.call(self.sigs.ret, h_return as usize, &[self.env, s]).unwrap();
                self.b.ins().return_(&[r]);
            }
            Op::ReturnNull => {
                let r = self.call(self.sigs.env_only, h_return_null as usize, &[self.env]).unwrap();
                self.b.ins().return_(&[r]);
            }
            _ => self.step(i),
        }
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

fn float_cc(op: BinOp) -> FloatCC {
    match op {
        BinOp::Gt => FloatCC::GreaterThan,
        BinOp::Lt => FloatCC::LessThan,
        BinOp::Ge => FloatCC::GreaterThanOrEqual,
        _ => FloatCC::LessThanOrEqual,
    }
}
