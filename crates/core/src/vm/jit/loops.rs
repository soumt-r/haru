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
//!
//! A loop may also call functions of the program and return: a call writes
//! its arguments back and reads its result, a return pops the frame itself
//! (the copy's registers hold nothing to release). A copy of a whole
//! function body was tried: loading and writing back at every call cost what
//! it saved (fib(30) 40 -> 48ms), the frame's own push and pop being most of
//! a call.
//!
//! A number is held as an `f64` (its bits only matter for other values), and
//! what is certainly a number or a boolean at each instruction (`analyze`:
//! nothing is known at the loop head, a guard that passed or a result
//! written makes it known) is not checked again.

use std::collections::HashMap;

use super::*;

/// What is certainly in some places: their tags.
type Known = HashMap<At, i64>;

/// A loop that gets a copy: its instructions `head..=last`.
pub(super) struct Region {
    pub(super) head: usize,
    last: usize,
    /// The registers and globals it uses, and what they are held in: the
    /// tag, the payload's bits, and the number (when the tag says one).
    slots: Vec<At>,
    vars: HashMap<At, (Variable, Variable, Variable)>,
    /// What is certain before each instruction (None: never reached).
    known: Vec<Option<Known>>,
    /// That of the instruction being compiled.
    cur: Known,
    /// The meta slots of the typed variables it updates (checked on entry).
    metas: Vec<At>,
    /// The copy's block of each instruction.
    spec: Vec<Block>,
    /// Where the copy leaves for the ordinary code at a pc.
    exits: HashMap<usize, Block>,
    /// The ordinary code of the head past the way in (a copy leaving at its
    /// head must not come straight back in).
    pub(super) ordinary: Option<Block>,
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
        Op::Enter | Op::Leave | Op::ReturnNull => vec![],
        Op::Return { src } => vec![R(src)],
        Op::Call { dst, proto, base, argc } if inline_call(prog, proto, argc) => {
            let mut v = vec![R(dst)];
            v.extend((base..base + argc).map(R));
            v
        }
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
        let vars = slots
            .iter()
            .map(|&at| (at, (self.b.declare_var(types::I32), self.b.declare_var(types::I64), self.b.declare_var(types::F64))))
            .collect();
        let spec = (head..=last).map(|_| self.b.create_block()).collect();
        let known = self.analyze(head, last);
        Region { head, last, slots, vars, known, cur: Known::new(), metas, spec, exits: HashMap::new(), ordinary: None }
    }

    /// What is certain before each instruction of `head..=last`: a forward
    /// pass to a fixed point, keeping what every way in agrees on. Nothing is
    /// known at the head (values come in from memory).
    fn analyze(&self, head: usize, last: usize) -> Vec<Option<Known>> {
        let mut state: Vec<Option<Known>> = vec![None; last - head + 1];
        state[0] = Some(Known::new());
        let mut changed = true;
        while changed {
            changed = false;
            for pc in head..=last {
                let Some(before) = state[pc - head].clone() else { continue };
                for (succ, after) in self.after(pc, before) {
                    if !(head..=last).contains(&succ) {
                        continue;
                    }
                    let merged = match &state[succ - head] {
                        None => after,
                        Some(old) => old.iter().filter(|(at, t)| after.get(at) == Some(t)).map(|(a, t)| (*a, *t)).collect(),
                    };
                    if state[succ - head].as_ref() != Some(&merged) {
                        state[succ - head] = Some(merged);
                        changed = true;
                    }
                }
            }
        }
        state
    }

    /// Where instruction `pc` goes on, with what is certain there.
    fn after(&self, pc: usize, mut k: Known) -> Vec<(usize, Known)> {
        use At::{Global as G, Reg as R};
        fn copy(k: &mut Known, dst: At, src: At) {
            match k.get(&src).copied() {
                Some(t) => k.insert(dst, t),
                None => k.remove(&dst),
            };
        }
        let target = |var: u32| match self.prog.vars[var as usize].slots[0].loc {
            Loc::Reg(r) => R(r),
            Loc::Global(g) => G(g),
            Loc::This(_) => unreachable!(),
        };
        let next = pc + 1;
        match self.proto.code[pc] {
            Op::LoadK { dst, k: c } => {
                let t = match self.prog.consts[c as usize].tag() {
                    tag::NUM => TAG_NUM,
                    tag::BOOL => TAG_BOOL,
                    _ => TAG_NULL,
                };
                k.insert(R(dst), t);
            }
            Op::LoadNull { dst } => {
                k.insert(R(dst), TAG_NULL);
            }
            Op::LoadBool { dst, .. } => {
                k.insert(R(dst), TAG_BOOL);
            }
            Op::Move { dst, src } => copy(&mut k, R(dst), R(src)),
            Op::GetReg { dst, slot, .. } => copy(&mut k, R(dst), R(slot)),
            Op::GetGlobal { dst, slot, .. } => copy(&mut k, R(dst), G(slot)),
            Op::SetReg { slot, src } => {
                if slot != src {
                    copy(&mut k, R(slot), R(src));
                    k.insert(R(src), TAG_UNDEF);
                }
            }
            Op::SetGlobal { slot, src } => {
                copy(&mut k, G(slot), R(src));
                k.insert(R(src), TAG_UNDEF);
            }
            Op::Bin { op, dst, a, b } => {
                k.insert(R(a), TAG_NUM);
                k.insert(R(b), TAG_NUM);
                k.insert(R(dst), if op.is_comparison() { TAG_BOOL } else { TAG_NUM });
            }
            Op::BinK { op, dst, a, .. } => {
                k.insert(R(a), TAG_NUM);
                k.insert(R(dst), if op.is_comparison() { TAG_BOOL } else { TAG_NUM });
            }
            Op::Eq { dst, .. } => {
                k.insert(R(dst), TAG_BOOL);
            }
            Op::EqK { dst, a, .. } => {
                k.insert(R(a), TAG_NUM);
                k.insert(R(dst), TAG_BOOL);
            }
            Op::Truth { dst, src } => {
                k.insert(R(src), TAG_BOOL);
                k.insert(R(dst), TAG_BOOL);
            }
            Op::Jump { to } => return vec![(to as usize, k)],
            Op::JumpIfFalse { cond, to } | Op::JumpIfTrue { cond, to } => {
                k.insert(R(cond), TAG_BOOL);
                return vec![(next, k.clone()), (to as usize, k)];
            }
            Op::CmpJump { a, b, to, .. } => {
                k.insert(R(a), TAG_NUM);
                k.insert(R(b), TAG_NUM);
                return vec![(next, k.clone()), (to as usize, k)];
            }
            Op::CmpKJump { a, to, .. } => {
                k.insert(R(a), TAG_NUM);
                return vec![(next, k.clone()), (to as usize, k)];
            }
            Op::EqJump { to, .. } => return vec![(pc + 2, k.clone()), (to as usize, k)],
            Op::EqKJump { a, to, .. } => {
                k.insert(R(a), TAG_NUM);
                return vec![(pc + 2, k.clone()), (to as usize, k)];
            }
            Op::RangePrep { start, end, step } => {
                for at in [start, end, step] {
                    k.insert(R(at), TAG_NUM);
                }
            }
            Op::RangeTest { exit, .. } => return vec![(next, k.clone()), (exit as usize, k)],
            Op::RangeStep { v, .. } => {
                k.insert(R(v), TAG_NUM);
            }
            Op::RangeNext { v, body, .. } => {
                k.insert(R(v), TAG_NUM);
                return vec![(next, k.clone()), (body as usize, k)];
            }
            Op::Boxed { .. } => {}
            Op::Undef { from, to } => {
                for r in from..to {
                    k.insert(R(r), TAG_UNDEF);
                }
            }
            Op::Update { var, a, b, .. } => {
                k.insert(R(a), TAG_NUM);
                k.insert(R(b), TAG_NUM);
                k.insert(target(var), TAG_NUM);
            }
            Op::UpdateK { var, skip, .. } => {
                let mut taken = k.clone();
                taken.insert(target(var), TAG_NUM);
                return vec![(skip as usize, taken), (next, k)];
            }
            Op::Call { dst, base, argc, .. } => {
                // The arguments move to the callee; the result is anything.
                for r in base..base + argc {
                    k.insert(R(r), TAG_UNDEF);
                }
                k.remove(&R(dst));
            }
            Op::Return { .. } | Op::ReturnNull => return vec![],
            _ => {}
        }
        vec![(next, k)]
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
            let f = self.num_of(a);
            let (tv, bv, fv) = r.vars[&at];
            self.b.def_var(tv, t);
            self.b.def_var(bv, p);
            self.b.def_var(fv, f);
        }
        let first = r.spec[0];
        self.b.ins().jump(first, &[]);
    }

    /// The copy's code for every instruction of the loop.
    pub(super) fn emit_region(&mut self, r: &mut Region) {
        for pc in r.head..=r.last {
            let blk = r.spec[pc - r.head];
            self.switch(blk);
            r.cur = r.known[pc - r.head].clone().unwrap_or_default();
            self.spec_op(r, pc);
        }
        // The ways out, each writing the variables back.
        let exits: Vec<(usize, Block)> = r.exits.iter().map(|(&pc, &b)| (pc, b)).collect();
        for (pc, blk) in exits {
            self.switch(blk);
            for &at in &r.slots.clone() {
                self.writeback(r, at);
            }
            let target = match (pc == r.head, r.ordinary) {
                (true, Some(b)) => b,
                _ => self.blocks[pc],
            };
            self.b.ins().jump(target, &[]);
        }
    }

    /// Writes the value held for `at` to memory.
    fn writeback(&mut self, r: &Region, at: At) {
        let (t, bits) = self.value_of(r, at);
        let a = self.addr(at);
        let t = self.b.ins().uextend(types::I64, t);
        self.b.ins().store(flags(), t, a, 0);
        self.b.ins().store(flags(), bits, a, 8);
    }

    /// The tag and the payload's bits held for `at` (a number's are in its f64).
    fn value_of(&mut self, r: &Region, at: At) -> (V, V) {
        let (tv, bv, fv) = r.vars[&at];
        let t = self.b.use_var(tv);
        let other = self.b.use_var(bv);
        let f = self.b.use_var(fv);
        let fbits = self.b.ins().bitcast(types::I64, MemFlags::new(), f);
        let is_num = self.b.ins().icmp_imm(IntCC::Equal, t, TAG_NUM);
        let bits = self.b.ins().select(is_num, fbits, other);
        (t, bits)
    }

    /// Loads what memory holds for `at` into its variables.
    fn load_slot(&mut self, r: &Region, at: At) {
        let a = self.addr(at);
        let (t, p, f) = (self.tag_of(a), self.payload_of(a), self.num_of(a));
        let (tv, bv, fv) = r.vars[&at];
        self.b.def_var(tv, t);
        self.b.def_var(bv, p);
        self.b.def_var(fv, f);
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
        let (tv, bv, _) = r.vars[&at];
        self.b.def_var(tv, t);
        self.b.def_var(bv, bits);
    }

    /// `dst` becomes what `src` holds.
    fn copy_slot(&mut self, r: &Region, dst: At, src: At) {
        let (st, sb, sf) = r.vars[&src];
        let (dt, db, df) = r.vars[&dst];
        let (t, b, f) = (self.b.use_var(st), self.b.use_var(sb), self.b.use_var(sf));
        self.b.def_var(dt, t);
        self.b.def_var(db, b);
        self.b.def_var(df, f);
    }

    fn set_const(&mut self, r: &Region, at: At, t: i64, bits: i64) {
        if t == TAG_NUM {
            let n = self.b.ins().f64const(f64::from_bits(bits as u64));
            return self.set_num(r, at, n);
        }
        let t = self.b.ins().iconst(types::I32, t);
        let bits = self.b.ins().iconst(types::I64, bits);
        self.set(r, at, t, bits);
    }

    fn set_num(&mut self, r: &Region, at: At, n: V) {
        let (tv, _, fv) = r.vars[&at];
        let t = self.b.ins().iconst(types::I32, TAG_NUM);
        self.b.def_var(tv, t);
        self.b.def_var(fv, n);
    }

    fn set_bool(&mut self, r: &Region, at: At, c: V) {
        let t = self.b.ins().iconst(types::I32, TAG_BOOL);
        let bits = self.b.ins().uextend(types::I64, c);
        self.set(r, at, t, bits);
    }

    fn num(&mut self, r: &Region, at: At) -> V {
        self.b.use_var(r.vars[&at].2)
    }

    /// Goes on in a new block when `cond`, else leaves at `pc`.
    fn guard(&mut self, r: &mut Region, pc: usize, cond: V) {
        let out = self.exit(r, pc);
        let go = self.b.create_block();
        self.b.ins().brif(cond, go, &[], out, &[]);
        self.switch(go);
    }

    /// Whether `at` holds a `want` (a constant when that is certain).
    fn is(&mut self, r: &Region, at: At, want: i64) -> V {
        if let Some(&t) = r.cur.get(&at) {
            return self.b.ins().iconst(types::I8, (t == want) as i64);
        }
        let t = self.tv(r, at);
        self.b.ins().icmp_imm(IntCC::Equal, t, want)
    }

    fn known(&self, r: &Region, at: At, want: i64) -> bool {
        r.cur.get(&at) == Some(&want)
    }

    /// Both numbers (their values), else leaving at `pc`; what is certainly a
    /// number is not checked.
    fn nums(&mut self, r: &mut Region, pc: usize, a: At, b: Option<At>) -> (V, Option<V>) {
        let unsure: Vec<At> = [Some(a), b].into_iter().flatten().filter(|&at| !self.known(r, at, TAG_NUM)).collect();
        if !unsure.is_empty() {
            let mut ok = self.is(r, unsure[0], TAG_NUM);
            for &at in &unsure[1..] {
                let o = self.is(r, at, TAG_NUM);
                ok = self.b.ins().band(ok, o);
            }
            self.guard(r, pc, ok);
        }
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
                self.copy_slot(r, R(dst), R(src));
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
                let c = self.prog.consts[k as usize].as_num().unwrap();
                // Adding or taking a number other than 0 never gives -0.
                if matches!(op, BinOp::Add | BinOp::Sub) && c != 0.0 {
                    let y = self.b.ins().f64const(c);
                    let n = if op == BinOp::Add { self.b.ins().fadd(x, y) } else { self.b.ins().fsub(x, y) };
                    self.set_num(r, R(dst), n);
                    return self.spec_next(r, pc);
                }
                let y = self.b.ins().f64const(c);
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
                if !self.known(r, target, TAG_NUM) {
                    let is_num = self.is(r, target, TAG_NUM);
                    let (go, next) = (self.b.create_block(), self.to(r, pc + 1));
                    self.b.ins().brif(is_num, go, &[], next, &[]);
                    self.switch(go);
                }
                let x = self.num(r, target);
                let c = self.prog.consts[k as usize].as_num().unwrap();
                let y = self.b.ins().f64const(c);
                let n = if op == BinOp::Add { self.b.ins().fadd(x, y) } else { self.b.ins().fsub(x, y) };
                // Adding or taking a number other than 0 never gives -0.
                let n = if c != 0.0 { n } else { self.boxed(n) };
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
                if !self.known(r, R(src), TAG_BOOL) {
                    let ok = self.is(r, R(src), TAG_BOOL);
                    self.guard(r, pc, ok);
                }
                self.copy_slot(r, R(dst), R(src));
                self.spec_next(r, pc);
            }
            Op::Jump { to } => {
                let t = self.to(r, to as usize);
                self.b.ins().jump(t, &[]);
            }
            Op::JumpIfFalse { cond, to } | Op::JumpIfTrue { cond, to } => {
                let on_true = matches!(self.proto.code[pc], Op::JumpIfTrue { .. });
                if !self.known(r, R(cond), TAG_BOOL) {
                    let ok = self.is(r, R(cond), TAG_BOOL);
                    self.guard(r, pc, ok);
                }
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
                // The f64 matters only when the tag says a number.
                let n = self.num(r, R(dst));
                let boxed = self.boxed(n);
                self.b.def_var(r.vars[&R(dst)].2, boxed);
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
            Op::Enter => {
                let d = self.b.ins().load(types::I32, flags(), self.vm, OFF_DEPTH as i32);
                let fine = self.b.ins().icmp_imm(IntCC::UnsignedLessThan, d, MAX_CALL_DEPTH as i64);
                self.guard(r, pc, fine);
                let d = self.b.ins().iadd_imm(d, 1);
                self.b.ins().store(flags(), d, self.vm, OFF_DEPTH as i32);
                self.spec_next(r, pc);
            }
            Op::Leave => {
                let d = self.b.ins().load(types::I32, flags(), self.vm, OFF_DEPTH as i32);
                let d = self.b.ins().iadd_imm(d, -1);
                self.b.ins().store(flags(), d, self.vm, OFF_DEPTH as i32);
                self.spec_next(r, pc);
            }
            Op::Call { dst, proto, base, argc } => self.spec_call(r, pc, dst, proto, base, argc),
            Op::Return { src } => self.spec_return(r, pc, Some(src)),
            Op::ReturnNull => self.spec_return(r, pc, None),
            _ => unreachable!("not an instruction a loop copy does"),
        }
    }

    /// A call: the arguments go to memory, the ordinary call's native start
    /// runs (anything unusual leaves here, everything written back), and the
    /// result comes back into the variables; a result that holds a
    /// reference leaves after the call (the copy keeps none).
    fn spec_call(&mut self, r: &mut Region, pc: usize, dst: Reg, proto: u32, base: Reg, argc: u16) {
        for k in 0..argc {
            self.writeback(r, At::Reg(base + k));
        }
        let slow = self.exit(r, pc);
        let status = self.call_core(dst, proto, base, argc, slow);
        let (ok, bad) = (self.b.create_block(), self.b.create_block());
        self.b.ins().brif(status, bad, &[], ok, &[]);

        // The callee failed: back to the ordinary code with everything in
        // memory (the arguments were moved out).
        self.switch(bad);
        self.reload();
        for k in 0..argc {
            self.set_const(r, At::Reg(base + k), TAG_UNDEF, 0);
        }
        for &at in &r.slots.clone() {
            self.writeback(r, at);
        }
        let (pcv, one) = (self.b.ins().iconst(types::I32, pc as i64), self.b.ins().iconst(types::I32, 1));
        let res = self.call(self.sigs.failed, h_call_failed as usize, &[self.env, pcv, one]).unwrap();
        self.go_on(pc, res);

        self.switch(ok);
        self.reload();
        for k in 0..argc {
            self.set_const(r, At::Reg(base + k), TAG_UNDEF, 0);
        }
        self.load_slot(r, At::Reg(dst));
        let t = self.tv(r, At::Reg(dst));
        let off = self.b.ins().iadd_imm(t, -(tag::STR as i64));
        let plain = self.b.ins().icmp_imm(IntCC::UnsignedGreaterThan, off, (tag::RESOURCE - tag::STR) as i64);
        self.guard(r, pc + 1, plain);
        self.spec_next(r, pc);
    }

    /// `돌려주자` (or the end, `src` None) of a call that only wants the value
    /// (or nothing): the frame ends here; anything else leaves for the
    /// ordinary return. The copy's registers hold nothing to release; the
    /// others (not used by the copy) are released.
    fn spec_return(&mut self, r: &mut Region, pc: usize, src: Option<Reg>) {
        let out = self.exit(r, pc);
        let (vm, ptr) = (self.vm, self.ptr);
        let flen = self.b.ins().load(ptr, flags(), vm, (OFF_FRAMES + OFF_LEN) as i32);
        let fp = self.b.ins().load(ptr, flags(), vm, (OFF_FRAMES + OFF_PTR) as i32);
        let top = self.b.ins().iadd_imm(flen, -1);
        let foff = self.b.ins().imul_imm(top, FRAME_SIZE as i64);
        let f = self.b.ins().iadd(fp, foff);
        let post = self.b.ins().load(types::I32, flags(), f, F_POST as i32);
        let pending = self.b.ins().load(ptr, flags(), f, F_PENDING as i32);
        let has_caller = self.b.ins().icmp_imm(IntCC::UnsignedGreaterThanOrEqual, flen, 2);
        let post_ok = match src {
            Some(_) => self.b.ins().icmp_imm(IntCC::Equal, post, 0),
            None => self.b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, post, 1),
        };
        let pend_ok = self.b.ins().icmp_imm(IntCC::Equal, pending, 0);
        let ok = self.b.ins().band(post_ok, pend_ok);
        let ok = self.b.ins().band(ok, has_caller);
        let go = self.b.create_block();
        self.b.ins().brif(ok, go, &[], out, &[]);
        self.switch(go);
        let (vt, vp) = match src {
            Some(src) => {
                let at = At::Reg(src);
                match simple_type(self.prog, self.proto.return_type) {
                    Some(tags) if !tags.is_empty() => {
                        let mut fits = self.b.ins().iconst(types::I8, 0);
                        for &want in tags {
                            let c = self.is(r, at, want);
                            fits = self.b.ins().bor(fits, c);
                        }
                        let fine = self.b.create_block();
                        self.b.ins().brif(fits, fine, &[], out, &[]);
                        self.switch(fine);
                    }
                    Some(_) => {}
                    None => {
                        self.b.ins().jump(out, &[]);
                        let dead = self.b.create_block();
                        self.switch(dead);
                    }
                }
                let (t, bits) = self.value_of(r, at);
                (self.b.ins().uextend(types::I64, t), bits)
            }
            None => (self.b.ins().iconst(types::I64, TAG_NULL), self.b.ins().iconst(types::I64, 0)),
        };
        // Registers the copy does not use, and a method's object.
        for reg in 0..self.proto.nregs {
            if !r.slots.contains(&At::Reg(reg)) {
                let a = self.addr(At::Reg(reg));
                self.release(a);
            }
        }
        let this = self.b.ins().iadd_imm(f, F_THIS as i64);
        self.release(this);
        let counted = self.b.ins().load(types::I8, flags(), f, F_COUNTED as i32);
        let counted = self.b.ins().uextend(types::I32, counted);
        let d = self.b.ins().load(types::I32, flags(), vm, OFF_DEPTH as i32);
        let d = self.b.ins().isub(d, counted);
        self.b.ins().store(flags(), d, vm, OFF_DEPTH as i32);
        let fbase = self.b.ins().load(ptr, flags(), f, F_BASE as i32);
        self.b.ins().store(flags(), fbase, vm, (OFF_STACK + OFF_LEN) as i32);
        self.b.ins().store(flags(), top, vm, (OFF_FRAMES + OFF_LEN) as i32);
        let ret = self.b.ins().load(types::I16, flags(), f, F_RET as i32);
        let ret = self.b.ins().uextend(ptr, ret);
        let caller = self.b.ins().iadd_imm(f, -(FRAME_SIZE as i64));
        let cbase = self.b.ins().load(ptr, flags(), caller, F_BASE as i32);
        let slot = self.b.ins().iadd(cbase, ret);
        let slot = self.b.ins().ishl_imm(slot, 4);
        let sp = self.b.ins().load(ptr, flags(), vm, (OFF_STACK + OFF_PTR) as i32);
        let dst = self.b.ins().iadd(sp, slot);
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
        let status = self.b.ins().iconst(types::I32, S_RETURNED as i64);
        self.b.ins().return_(&[status]);
    }

    /// `GetReg` / `GetGlobal`: an undefined variable leaves (the ordinary
    /// code reports it).
    fn spec_get(&mut self, r: &mut Region, pc: usize, dst: Reg, src: At) {
        if !matches!(r.cur.get(&src), Some(&t) if t != TAG_UNDEF) {
            let t = self.tv(r, src);
            let defined = self.b.ins().icmp_imm(IntCC::NotEqual, t, TAG_UNDEF);
            self.guard(r, pc, defined);
        }
        self.copy_slot(r, At::Reg(dst), src);
        self.spec_next(r, pc);
    }

    /// `dst = take(src)`.
    fn spec_take(&mut self, r: &Region, dst: At, src: Reg) {
        self.copy_slot(r, dst, At::Reg(src));
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
