//! Loops of numbers in CPU registers.
//!
//! A loop whose instructions all work on numbers and booleans (arithmetic,
//! comparisons, jumps, moving registers and globals, range loop steps,
//! `더하자` on a number variable) gets a second copy of its code that keeps
//! the tag and the bits of every register and global it uses in Cranelift
//! variables instead of memory. It is entered at the loop head when none of
//! them holds a counted value (so nothing needs releasing while they are in
//! variables) and the typed variables it updates take numbers. Anything the
//! copy does not expect (a value of another type, dividing by zero, an
//! undefined variable) and every way out of the loop write the variables
//! back and go on in the ordinary code at that very instruction, which does
//! what the interpreter does. So the copy never decides anything itself
//! that the ordinary code would not.

use std::collections::HashMap;

use super::*;

/// A loop that gets a copy: its instructions `head..=last`.
pub(super) struct Region {
    pub(super) head: usize,
    last: usize,
    /// The registers and globals it uses, and what they are held in (the
    /// tag, and the payload's bits).
    slots: Vec<At>,
    vars: HashMap<At, (Variable, Variable)>,
    /// The meta slots of the typed variables it updates (checked on entry).
    metas: Vec<At>,
    /// The copy's block of each instruction.
    spec: Vec<Block>,
    /// Where the copy leaves for the ordinary code at a pc.
    exits: HashMap<usize, Block>,
}

/// Whether the copy can do `op`, and the places it touches.
fn slots_of(op: &Op, prog: &Program) -> Option<Vec<At>> {
    use At::{Global as G, Reg as R};
    let num = |k: u32| prog.consts[k as usize].as_num().is_some();
    Some(match *op {
        Op::LoadK { dst, k } => {
            if !matches!(prog.consts[k as usize].tag(), tag::NUM | tag::BOOL | tag::NULL) {
                return None;
            }
            vec![R(dst)]
        }
        Op::LoadNull { dst } | Op::LoadBool { dst, .. } | Op::Boxed { dst } => vec![R(dst)],
        Op::Move { dst, src } => vec![R(dst), R(src)],
        Op::GetReg { dst, slot, .. } => vec![R(dst), R(slot)],
        Op::GetGlobal { dst, slot, .. } => vec![R(dst), G(slot)],
        Op::SetReg { slot, src } => vec![R(slot), R(src)],
        Op::SetGlobal { slot, src } => vec![G(slot), R(src)],
        Op::Bin { dst, a, b, .. } | Op::Eq { dst, a, b, .. } => vec![R(dst), R(a), R(b)],
        Op::BinK { dst, a, k, .. } if num(k) => vec![R(dst), R(a)],
        Op::EqK { dst, a, k, .. } if num(k) => vec![R(dst), R(a)],
        Op::UpdateK { var, k, .. } if num(k) => match prog.vars[var as usize].slots.as_slice() {
            [Slot { loc: Loc::Reg(r), meta: None | Some(Loc::Reg(_) | Loc::Global(_)) }] => vec![R(*r)],
            [Slot { loc: Loc::Global(g), meta: None | Some(Loc::Reg(_) | Loc::Global(_)) }] => vec![G(*g)],
            _ => return None,
        },
        Op::EqJump { a, b, dst, .. } => vec![R(a), R(b), R(dst)],
        Op::EqKJump { a, k, dst, .. } if num(k) => vec![R(a), R(dst)],
        Op::Truth { dst, src } => vec![R(dst), R(src)],
        Op::Jump { .. } => vec![],
        Op::JumpIfFalse { cond, .. } | Op::JumpIfTrue { cond, .. } => vec![R(cond)],
        Op::CmpJump { a, b, .. } => vec![R(a), R(b)],
        Op::CmpKJump { a, k, .. } if num(k) => vec![R(a)],
        Op::RangePrep { start, end, step } => vec![R(start), R(end), R(step)],
        Op::RangeTest { v, end, step, .. } => vec![R(v), R(end), R(step)],
        Op::RangeStep { v, step } => vec![R(v), R(step)],
        Op::RangeNext { v, end, step, .. } => vec![R(v), R(end), R(step)],
        Op::Undef { from, to } => (from..to).map(R).collect(),
        Op::Update { var, a, b, op: BinOp::Add | BinOp::Sub } => match prog.vars[var as usize].slots.as_slice() {
            [Slot { loc, meta }] if matches!(meta, None | Some(Loc::Reg(_) | Loc::Global(_))) => {
                let target = match loc {
                    Loc::Reg(r) => R(*r),
                    Loc::Global(g) => G(*g),
                    Loc::This(_) => return None,
                };
                vec![R(a), R(b), target]
            }
            _ => return None,
        },
        _ => return None,
    })
}

/// The meta slot of a typed variable an `Update` changes, if any.
fn update_meta(op: &Op, prog: &Program) -> Option<At> {
    match *op {
        Op::Update { var, .. } | Op::UpdateK { var, .. } => match prog.vars[var as usize].slots.first()?.meta? {
            Loc::Reg(r) => Some(At::Reg(r)),
            Loc::Global(g) => Some(At::Global(g)),
            Loc::This(_) => None,
        },
        _ => None,
    }
}

/// The loops of `code` that get a copy: the outermost ones whose every
/// instruction the copy can do.
pub(super) fn find(code: &[Op], prog: &Program) -> Vec<(usize, usize)> {
    let mut loops: Vec<(usize, usize)> = code
        .iter()
        .enumerate()
        .filter_map(|(j, op)| match *op {
            Op::Jump { to } if (to as usize) < j => Some((to as usize, j)),
            Op::RangeNext { body, .. } if (body as usize) <= j => Some((body as usize, j)),
            _ => None,
        })
        .collect();
    // Biggest first, so an outer loop is taken before the loops inside it.
    loops.sort_by_key(|&(h, j)| std::cmp::Reverse(j - h));
    let mut taken: Vec<(usize, usize)> = Vec::new();
    for (h, j) in loops {
        if taken.iter().any(|&(th, tj)| h <= tj && th <= j) {
            continue;
        }
        if code[h..=j].iter().all(|op| slots_of(op, prog).is_some()) {
            taken.push((h, j));
        }
    }
    taken
}

impl Gen<'_, '_> {
    pub(super) fn region(&mut self, head: usize, last: usize) -> Region {
        let mut slots = Vec::new();
        let mut metas = Vec::new();
        for op in &self.proto.code[head..=last] {
            for at in slots_of(op, self.prog).unwrap() {
                if !slots.contains(&at) {
                    slots.push(at);
                }
            }
            if let Some(m) = update_meta(op, self.prog) {
                if !metas.contains(&m) {
                    metas.push(m);
                }
            }
        }
        let vars = slots.iter().map(|&at| (at, (self.b.declare_var(types::I32), self.b.declare_var(types::I64)))).collect();
        let spec = (head..=last).map(|_| self.b.create_block()).collect();
        Region { head, last, slots, vars, metas, spec, exits: HashMap::new() }
    }

    /// At the loop head in the ordinary code: into the copy when its values
    /// allow it, else on to `ordinary`.
    pub(super) fn enter_region(&mut self, r: &Region, ordinary: Block) {
        let counted_max = (tag::RESOURCE - tag::STR) as i64;
        let mut bad = self.b.ins().iconst(types::I8, 0);
        for &at in &r.slots {
            let a = self.addr(at);
            let t = self.tag_of(a);
            let off = self.b.ins().iadd_imm(t, -(tag::STR as i64));
            let counted = self.b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, off, counted_max);
            bad = self.b.ins().bor(bad, counted);
        }
        // A typed variable updated in the loop must take numbers (and not be
        // a constant): no type, or a number type's id (see `number_types`).
        for &m in &r.metas {
            let a = self.addr(m);
            let t = self.tag_of(a);
            let is_num = self.is_tag(t, TAG_NUM);
            let u = self.is_tag(t, TAG_UNDEF);
            let n = self.is_tag(t, TAG_NULL);
            let untyped = self.b.ins().bor(u, n);
            let v = self.num_of(a);
            let mut fits = self.b.ins().iconst(types::I8, 0);
            for id in number_types(self.prog) {
                let k = self.b.ins().f64const((id * 2) as f64);
                let c = self.b.ins().fcmp(FloatCC::Equal, v, k);
                fits = self.b.ins().bor(fits, c);
            }
            let typed_ok = self.b.ins().band(is_num, fits);
            let ok = self.b.ins().bor(untyped, typed_ok);
            let not_ok = self.b.ins().bxor_imm(ok, 1);
            bad = self.b.ins().bor(bad, not_ok);
        }
        let enter = self.b.create_block();
        self.b.ins().brif(bad, ordinary, &[], enter, &[]);
        self.switch(enter);
        for &at in &r.slots {
            let a = self.addr(at);
            let t = self.tag_of(a);
            let p = self.payload_of(a);
            let (tv, bv) = r.vars[&at];
            self.b.def_var(tv, t);
            self.b.def_var(bv, p);
        }
        let first = r.spec[0];
        self.b.ins().jump(first, &[]);
    }

    /// The copy's code for every instruction of the loop.
    pub(super) fn emit_region(&mut self, r: &mut Region) {
        for pc in r.head..=r.last {
            let blk = r.spec[pc - r.head];
            self.switch(blk);
            self.spec_op(r, pc);
        }
        // The ways out, each writing the variables back.
        let exits: Vec<(usize, Block)> = r.exits.iter().map(|(&pc, &b)| (pc, b)).collect();
        for (pc, blk) in exits {
            self.switch(blk);
            for &at in &r.slots {
                let (tv, bv) = r.vars[&at];
                let t = self.b.use_var(tv);
                let bits = self.b.use_var(bv);
                let a = self.addr(at);
                let t = self.b.ins().uextend(types::I64, t);
                self.b.ins().store(flags(), t, a, 0);
                self.b.ins().store(flags(), bits, a, 8);
            }
            let target = self.blocks[pc];
            self.b.ins().jump(target, &[]);
        }
    }

    /// Where the copy goes to run instruction `pc`: its own block inside the
    /// loop, else the ordinary code (after writing back).
    fn to(&mut self, r: &mut Region, pc: usize) -> Block {
        if (r.head..=r.last).contains(&pc) {
            return r.spec[pc - r.head];
        }
        self.exit(r, pc)
    }

    /// The ordinary code at `pc`, after writing back.
    fn exit(&mut self, r: &mut Region, pc: usize) -> Block {
        if let Some(&b) = r.exits.get(&pc) {
            return b;
        }
        let b = self.b.create_block();
        r.exits.insert(pc, b);
        b
    }

    fn tv(&mut self, r: &Region, at: At) -> V {
        self.b.use_var(r.vars[&at].0)
    }

    fn bv(&mut self, r: &Region, at: At) -> V {
        self.b.use_var(r.vars[&at].1)
    }

    fn set(&mut self, r: &Region, at: At, t: V, bits: V) {
        let (tv, bv) = r.vars[&at];
        self.b.def_var(tv, t);
        self.b.def_var(bv, bits);
    }

    fn set_const(&mut self, r: &Region, at: At, t: i64, bits: i64) {
        let t = self.b.ins().iconst(types::I32, t);
        let bits = self.b.ins().iconst(types::I64, bits);
        self.set(r, at, t, bits);
    }

    fn set_num(&mut self, r: &Region, at: At, n: V) {
        let t = self.b.ins().iconst(types::I32, TAG_NUM);
        let bits = self.b.ins().bitcast(types::I64, MemFlags::new(), n);
        self.set(r, at, t, bits);
    }

    fn set_bool(&mut self, r: &Region, at: At, c: V) {
        let t = self.b.ins().iconst(types::I32, TAG_BOOL);
        let bits = self.b.ins().uextend(types::I64, c);
        self.set(r, at, t, bits);
    }

    fn num(&mut self, r: &Region, at: At) -> V {
        let bits = self.bv(r, at);
        self.b.ins().bitcast(types::F64, MemFlags::new(), bits)
    }

    /// Goes on in a new block when `cond`, else leaves at `pc`.
    fn guard(&mut self, r: &mut Region, pc: usize, cond: V) {
        let out = self.exit(r, pc);
        let go = self.b.create_block();
        self.b.ins().brif(cond, go, &[], out, &[]);
        self.switch(go);
    }

    fn is(&mut self, r: &Region, at: At, want: i64) -> V {
        let t = self.tv(r, at);
        self.b.ins().icmp_imm(IntCC::Equal, t, want)
    }

    /// Both numbers (their values), else leaving at `pc`.
    fn nums(&mut self, r: &mut Region, pc: usize, a: At, b: Option<At>) -> (V, Option<V>) {
        let mut ok = self.is(r, a, TAG_NUM);
        if let Some(b) = b {
            let o = self.is(r, b, TAG_NUM);
            ok = self.b.ins().band(ok, o);
        }
        self.guard(r, pc, ok);
        let x = self.num(r, a);
        (x, b.map(|b| self.num(r, b)))
    }

    fn spec_next(&mut self, r: &mut Region, pc: usize) {
        let next = self.to(r, pc + 1);
        self.b.ins().jump(next, &[]);
    }

    fn spec_op(&mut self, r: &mut Region, pc: usize) {
        use At::{Global as G, Reg as R};
        match self.proto.code[pc] {
            Op::LoadK { dst, k } => {
                let c = &self.prog.consts[k as usize];
                let (t, bits) = match c.tag() {
                    tag::NUM => (TAG_NUM, c.as_num().unwrap().to_bits() as i64),
                    tag::BOOL => (TAG_BOOL, c.as_bool().unwrap() as i64),
                    _ => (TAG_NULL, 0),
                };
                self.set_const(r, R(dst), t, bits);
                self.spec_next(r, pc);
            }
            Op::LoadNull { dst } => {
                self.set_const(r, R(dst), TAG_NULL, 0);
                self.spec_next(r, pc);
            }
            Op::LoadBool { dst, v } => {
                self.set_const(r, R(dst), TAG_BOOL, v as i64);
                self.spec_next(r, pc);
            }
            Op::Move { dst, src } => {
                let (t, bits) = (self.tv(r, R(src)), self.bv(r, R(src)));
                self.set(r, R(dst), t, bits);
                self.spec_next(r, pc);
            }
            Op::GetReg { dst, slot, .. } => self.spec_get(r, pc, dst, R(slot)),
            Op::GetGlobal { dst, slot, .. } => self.spec_get(r, pc, dst, G(slot)),
            Op::SetReg { slot, src } => {
                if slot != src {
                    self.spec_take(r, R(slot), src);
                }
                self.spec_next(r, pc);
            }
            Op::SetGlobal { slot, src } => {
                self.spec_take(r, G(slot), src);
                self.spec_next(r, pc);
            }
            Op::Bin { op, dst, a, b } => {
                let (x, y) = self.nums(r, pc, R(a), Some(R(b)));
                self.spec_arith(r, pc, op, dst, x, y.unwrap());
            }
            Op::BinK { op, dst, a, k } => {
                let (x, _) = self.nums(r, pc, R(a), None);
                let y = self.b.ins().f64const(self.prog.consts[k as usize].as_num().unwrap());
                self.spec_arith(r, pc, op, dst, x, y);
            }
            Op::Eq { dst, a, b, neg } => {
                // Two numbers, or two booleans; anything else leaves.
                let (na, nb) = (self.is(r, R(a), TAG_NUM), self.is(r, R(b), TAG_NUM));
                let nums = self.b.ins().band(na, nb);
                let (ba, bb) = (self.is(r, R(a), TAG_BOOL), self.is(r, R(b), TAG_BOOL));
                let bools = self.b.ins().band(ba, bb);
                let either = self.b.ins().bor(nums, bools);
                self.guard(r, pc, either);
                let (x, y) = (self.num(r, R(a)), self.num(r, R(b)));
                let fe = self.b.ins().fcmp(FloatCC::Equal, x, y);
                let (bx, by) = (self.bv(r, R(a)), self.bv(r, R(b)));
                let be = self.b.ins().icmp(IntCC::Equal, bx, by);
                let eq = self.b.ins().select(nums, fe, be);
                let eq = if neg { self.b.ins().bxor_imm(eq, 1) } else { eq };
                self.set_bool(r, R(dst), eq);
                self.spec_next(r, pc);
            }
            Op::UpdateK { var, k, op, skip } => {
                // The meta was checked on entry; a number changes here, else
                // the instructions after it do it.
                let target = match self.prog.vars[var as usize].slots[0].loc {
                    Loc::Reg(reg) => R(reg),
                    Loc::Global(g) => G(g),
                    Loc::This(_) => unreachable!(),
                };
                let is_num = self.is(r, target, TAG_NUM);
                let (go, next) = (self.b.create_block(), self.to(r, pc + 1));
                self.b.ins().brif(is_num, go, &[], next, &[]);
                self.switch(go);
                let x = self.num(r, target);
                let y = self.b.ins().f64const(self.prog.consts[k as usize].as_num().unwrap());
                let n = if op == BinOp::Add { self.b.ins().fadd(x, y) } else { self.b.ins().fsub(x, y) };
                let n = self.boxed(n);
                self.set_num(r, target, n);
                let skip = self.to(r, skip as usize);
                self.b.ins().jump(skip, &[]);
            }
            Op::EqK { dst, a, k, neg } => {
                let (x, _) = self.nums(r, pc, R(a), None);
                let y = self.b.ins().f64const(self.prog.consts[k as usize].as_num().unwrap());
                let cc = if neg { FloatCC::NotEqual } else { FloatCC::Equal };
                let c = self.b.ins().fcmp(cc, x, y);
                self.set_bool(r, R(dst), c);
                self.spec_next(r, pc);
            }
            Op::EqJump { a, b, neg, to, .. } => {
                // Two numbers, or two booleans; anything else leaves.
                let (na, nb) = (self.is(r, R(a), TAG_NUM), self.is(r, R(b), TAG_NUM));
                let nums = self.b.ins().band(na, nb);
                let (ba, bb) = (self.is(r, R(a), TAG_BOOL), self.is(r, R(b), TAG_BOOL));
                let bools = self.b.ins().band(ba, bb);
                let either = self.b.ins().bor(nums, bools);
                self.guard(r, pc, either);
                let (x, y) = (self.num(r, R(a)), self.num(r, R(b)));
                let fe = self.b.ins().fcmp(FloatCC::Equal, x, y);
                let (bx, by) = (self.bv(r, R(a)), self.bv(r, R(b)));
                let be = self.b.ins().icmp(IntCC::Equal, bx, by);
                let eq = self.b.ins().select(nums, fe, be);
                let eq = if neg { self.b.ins().bxor_imm(eq, 1) } else { eq };
                let (past, to) = (self.to(r, pc + 2), self.to(r, to as usize));
                self.b.ins().brif(eq, past, &[], to, &[]);
            }
            Op::EqKJump { a, k, neg, to, .. } => {
                let (x, _) = self.nums(r, pc, R(a), None);
                let y = self.b.ins().f64const(self.prog.consts[k as usize].as_num().unwrap());
                let cc = if neg { FloatCC::NotEqual } else { FloatCC::Equal };
                let c = self.b.ins().fcmp(cc, x, y);
                let (past, to) = (self.to(r, pc + 2), self.to(r, to as usize));
                self.b.ins().brif(c, past, &[], to, &[]);
            }
            Op::Truth { dst, src } => {
                let ok = self.is(r, R(src), TAG_BOOL);
                self.guard(r, pc, ok);
                let (t, bits) = (self.tv(r, R(src)), self.bv(r, R(src)));
                self.set(r, R(dst), t, bits);
                self.spec_next(r, pc);
            }
            Op::Jump { to } => {
                let t = self.to(r, to as usize);
                self.b.ins().jump(t, &[]);
            }
            Op::JumpIfFalse { cond, to } | Op::JumpIfTrue { cond, to } => {
                let on_true = matches!(self.proto.code[pc], Op::JumpIfTrue { .. });
                let ok = self.is(r, R(cond), TAG_BOOL);
                self.guard(r, pc, ok);
                let bits = self.bv(r, R(cond));
                let (to, next) = (self.to(r, to as usize), self.to(r, pc + 1));
                if on_true {
                    self.b.ins().brif(bits, to, &[], next, &[]);
                } else {
                    self.b.ins().brif(bits, next, &[], to, &[]);
                }
            }
            Op::CmpJump { op, a, b, to } => {
                let (x, y) = self.nums(r, pc, R(a), Some(R(b)));
                let c = self.b.ins().fcmp(float_cc(op), x, y.unwrap());
                let (to, next) = (self.to(r, to as usize), self.to(r, pc + 1));
                self.b.ins().brif(c, next, &[], to, &[]);
            }
            Op::CmpKJump { op, a, k, to } => {
                let (x, _) = self.nums(r, pc, R(a), None);
                let y = self.b.ins().f64const(self.prog.consts[k as usize].as_num().unwrap());
                let c = self.b.ins().fcmp(float_cc(op), x, y);
                let (to, next) = (self.to(r, to as usize), self.to(r, pc + 1));
                self.b.ins().brif(c, next, &[], to, &[]);
            }
            Op::RangePrep { start, end, step } => {
                let (s, e) = self.nums(r, pc, R(start), Some(R(end)));
                let down = self.b.ins().fcmp(FloatCC::GreaterThan, s, e.unwrap());
                let (m1, p1) = (self.b.ins().f64const(-1.0), self.b.ins().f64const(1.0));
                let v = self.b.ins().select(down, m1, p1);
                self.set_num(r, R(step), v);
                self.spec_next(r, pc);
            }
            Op::RangeTest { v, end, step, exit } => {
                // The loop's own registers: numbers (RangePrep made sure).
                let (v, e, s) = (self.num(r, R(v)), self.num(r, R(end)), self.num(r, R(step)));
                let zero = self.b.ins().f64const(0.0);
                let up = self.b.ins().fcmp(FloatCC::GreaterThan, s, zero);
                let past_up = self.b.ins().fcmp(FloatCC::GreaterThan, v, e);
                let a = self.b.ins().band(up, past_up);
                let down = self.b.ins().fcmp(FloatCC::LessThan, s, zero);
                let past_down = self.b.ins().fcmp(FloatCC::LessThan, v, e);
                let c = self.b.ins().band(down, past_down);
                let out = self.b.ins().bor(a, c);
                let (exit, next) = (self.to(r, exit as usize), self.to(r, pc + 1));
                self.b.ins().brif(out, exit, &[], next, &[]);
            }
            Op::RangeNext { v, end, step, body } => {
                let (x, e, s) = (self.num(r, R(v)), self.num(r, R(end)), self.num(r, R(step)));
                let n = self.b.ins().fadd(x, s);
                self.set_num(r, R(v), n);
                let zero = self.b.ins().f64const(0.0);
                let up = self.b.ins().fcmp(FloatCC::GreaterThan, s, zero);
                let past_up = self.b.ins().fcmp(FloatCC::GreaterThan, n, e);
                let a = self.b.ins().band(up, past_up);
                let down = self.b.ins().fcmp(FloatCC::LessThan, s, zero);
                let past_down = self.b.ins().fcmp(FloatCC::LessThan, n, e);
                let c = self.b.ins().band(down, past_down);
                let out = self.b.ins().bor(a, c);
                let (next, body) = (self.to(r, pc + 1), self.to(r, body as usize));
                self.b.ins().brif(out, next, &[], body, &[]);
            }
            Op::RangeStep { v, step } => {
                let (x, s) = (self.num(r, R(v)), self.num(r, R(step)));
                let n = self.b.ins().fadd(x, s);
                self.set_num(r, R(v), n);
                self.spec_next(r, pc);
            }
            Op::Boxed { dst } => {
                let is_num = self.is(r, R(dst), TAG_NUM);
                let n = self.num(r, R(dst));
                let boxed = self.boxed(n);
                let boxed = self.b.ins().bitcast(types::I64, MemFlags::new(), boxed);
                let old = self.bv(r, R(dst));
                let bits = self.b.ins().select(is_num, boxed, old);
                let t = self.tv(r, R(dst));
                self.set(r, R(dst), t, bits);
                self.spec_next(r, pc);
            }
            Op::Undef { from, to } => {
                for reg in from..to {
                    self.set_const(r, R(reg), TAG_UNDEF, 0);
                }
                self.spec_next(r, pc);
            }
            Op::Update { var, a, b, op } => {
                let target = match self.prog.vars[var as usize].slots[0].loc {
                    Loc::Reg(reg) => R(reg),
                    Loc::Global(g) => G(g),
                    Loc::This(_) => unreachable!(),
                };
                let (x, y) = self.nums(r, pc, R(a), Some(R(b)));
                let y = y.unwrap();
                let n = if op == BinOp::Add { self.b.ins().fadd(x, y) } else { self.b.ins().fsub(x, y) };
                let n = self.boxed(n);
                self.set_num(r, target, n);
                self.spec_next(r, pc);
            }
            _ => unreachable!("not an instruction a loop copy does"),
        }
    }

    /// `GetReg` / `GetGlobal`: an undefined variable leaves (the ordinary
    /// code reports it).
    fn spec_get(&mut self, r: &mut Region, pc: usize, dst: Reg, src: At) {
        let t = self.tv(r, src);
        let defined = self.b.ins().icmp_imm(IntCC::NotEqual, t, TAG_UNDEF);
        self.guard(r, pc, defined);
        let (t, bits) = (self.tv(r, src), self.bv(r, src));
        self.set(r, At::Reg(dst), t, bits);
        self.spec_next(r, pc);
    }

    /// `dst = take(src)`.
    fn spec_take(&mut self, r: &Region, dst: At, src: Reg) {
        let (t, bits) = (self.tv(r, At::Reg(src)), self.bv(r, At::Reg(src)));
        self.set(r, dst, t, bits);
        self.set_const(r, At::Reg(src), TAG_UNDEF, 0);
    }

    /// Arithmetic or a comparison of two numbers into `dst` (the cases the
    /// ordinary code leaves to the interpreter leave here too).
    fn spec_arith(&mut self, r: &mut Region, pc: usize, op: BinOp, dst: Reg, x: V, y: V) {
        let dst = At::Reg(dst);
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => {
                let n = match op {
                    BinOp::Add => self.b.ins().fadd(x, y),
                    BinOp::Sub => self.b.ins().fsub(x, y),
                    BinOp::Mul => self.b.ins().fmul(x, y),
                    _ => {
                        let zero = self.b.ins().f64const(0.0);
                        let nonzero = self.b.ins().fcmp(FloatCC::NotEqual, y, zero);
                        self.guard(r, pc, nonzero);
                        self.b.ins().fdiv(x, y)
                    }
                };
                let n = self.boxed(n);
                self.set_num(r, dst, n);
            }
            BinOp::Mod => {
                let lo = self.b.ins().f64const(-9_223_372_036_854_775_808.0);
                let hi = self.b.ins().f64const(9_223_372_036_854_775_808.0);
                let mut fits = self.b.ins().iconst(types::I8, 1);
                for v in [x, y] {
                    let a = self.b.ins().fcmp(FloatCC::GreaterThanOrEqual, v, lo);
                    let b = self.b.ins().fcmp(FloatCC::LessThan, v, hi);
                    let both = self.b.ins().band(a, b);
                    fits = self.b.ins().band(fits, both);
                }
                self.guard(r, pc, fits);
                let xi = self.b.ins().fcvt_to_sint_sat(types::I64, x);
                let yi = self.b.ins().fcvt_to_sint_sat(types::I64, y);
                let y1 = self.b.ins().iadd_imm(yi, 1);
                let fine = self.b.ins().icmp_imm(IntCC::UnsignedGreaterThan, y1, 1);
                self.guard(r, pc, fine);
                let m = self.b.ins().srem(xi, yi);
                let n = self.b.ins().fcvt_from_sint(types::F64, m);
                self.set_num(r, dst, n);
            }
            _ => {
                let c = self.b.ins().fcmp(float_cc(op), x, y);
                self.set_bool(r, dst, c);
            }
        }
        self.spec_next(r, pc);
    }
}
