//! AST → register code. The semantics are those of Hana's tree-walker
//! (`hana/vm`), which is the reference; comments name the Hana behaviour a
//! construct reproduces where it is not obvious.
//!
//! Scopes: the top level, each function call, each loop pass (and, later,
//! each catch handler) is a scope; `만약` and `따라 나누자` blocks are not.
//! A function sees its own scopes and the top level only (Hana has no
//! closures). Before compiling a scope the compiler collects the names it
//! can declare and gives each a slot, so a variable reference becomes the
//! list of slots it may be in ([`Var`]).

use std::collections::HashMap;

use haru_syntax::ast::{self, Block, Expr, Stmt};

use crate::bytecode::*;
use crate::lang::Lang;
use crate::value::{FuncObj, Value};

/// A construct this version cannot run yet.
#[derive(Debug)]
pub struct Unsupported(pub String);

pub fn compile(program: &ast::Program, lang: &'static Lang) -> Result<Program, Unsupported> {
    let mut c = Compiler {
        prog: Program {
            lang,
            protos: Vec::new(),
            consts: Vec::new(),
            names: Vec::new(),
            vars: Vec::new(),
            types: vec![TypeSpec::parse("", lang)],
            globals: Vec::new(),
            global_names: Vec::new(),
            functions: Vec::new(),
        },
        name_ids: HashMap::new(),
        type_ids: HashMap::new(),
        str_consts: HashMap::new(),
        globals: HashMap::new(),
        functions: HashMap::new(),
    };
    c.check_supported(&program.statements)?;

    // Built-ins are variables of the top level (a program may even replace them).
    for (i, name) in lang.builtins.iter().enumerate() {
        let slot = c.new_global(name, false);
        c.prog.globals[slot.loc_global() as usize] = Value::func(FuncObj::Builtin(i as u8));
    }
    for (name, flags) in declared_names(&program.statements) {
        if !c.globals.contains_key(&name) {
            c.new_global(&name, flags);
        } else if flags {
            let slot = c.globals.get_mut(&name).unwrap();
            if slot.meta.is_none() {
                let meta = c.prog.globals.len() as u32;
                c.prog.globals.push(Value::UNDEF);
                slot.meta = Some(Loc::Global(meta));
            }
        }
    }

    // Top-level functions: the first of each name (later ones are never found).
    let mut decls = Vec::new();
    for s in &program.statements {
        if let Stmt::Function(f) = s {
            if !c.functions.contains_key(&f.name) {
                let proto = (decls.len() + 1) as u32;
                c.functions.insert(f.name.clone(), proto);
                c.prog.functions.push((f.name.clone(), proto));
                decls.push(f);
            }
        }
    }

    c.prog.protos.push(placeholder("<main>"));
    for f in &decls {
        c.prog.protos.push(placeholder(&f.name));
    }
    let main = FnCompiler::new(&mut c, None).compile_main(&program.statements);
    c.prog.protos[0] = main;
    for (i, f) in decls.iter().enumerate() {
        let proto = FnCompiler::new(&mut c, Some(f)).compile_function(f);
        c.prog.protos[i + 1] = proto;
    }
    c.prog.global_names = c.globals.iter().map(|(n, s)| (n.clone(), s.loc_global())).collect();
    Ok(c.prog)
}

fn placeholder(name: &str) -> Proto {
    Proto {
        name: name.to_string(),
        code: Vec::new(),
        nregs: 0,
        params: Vec::new(),
        return_type: 0,
        loops: Vec::new(),
        handlers: Vec::new(),
    }
}

impl Slot {
    fn loc_global(&self) -> u32 {
        match self.loc {
            Loc::Global(g) => g,
            Loc::Reg(_) => unreachable!(),
        }
    }
}

struct Compiler {
    prog: Program,
    name_ids: HashMap<String, u32>,
    type_ids: HashMap<String, u32>,
    str_consts: HashMap<String, u32>,
    globals: HashMap<String, Slot>,
    functions: HashMap<String, u32>,
}

impl Compiler {
    fn name(&mut self, s: &str) -> u32 {
        if let Some(&i) = self.name_ids.get(s) {
            return i;
        }
        let i = self.prog.names.len() as u32;
        self.prog.names.push(s.to_string());
        self.name_ids.insert(s.to_string(), i);
        i
    }

    fn type_id(&mut self, text: &str) -> u32 {
        if let Some(&i) = self.type_ids.get(text) {
            return i;
        }
        let i = self.prog.types.len() as u32;
        self.prog.types.push(TypeSpec::parse(text, self.prog.lang));
        self.type_ids.insert(text.to_string(), i);
        i
    }

    fn str_const(&mut self, s: &str) -> u32 {
        if let Some(&i) = self.str_consts.get(s) {
            return i;
        }
        let i = self.prog.consts.len() as u32;
        self.prog.consts.push(Value::str(s));
        self.str_consts.insert(s.to_string(), i);
        i
    }

    fn num_const(&mut self, n: f64) -> u32 {
        let i = self.prog.consts.len() as u32;
        self.prog.consts.push(Value::num(n));
        i
    }

    fn new_global(&mut self, name: &str, meta: bool) -> Slot {
        let loc = Loc::Global(self.prog.globals.len() as u32);
        self.prog.globals.push(Value::UNDEF);
        let meta = meta.then(|| {
            let m = Loc::Global(self.prog.globals.len() as u32);
            self.prog.globals.push(Value::UNDEF);
            m
        });
        let slot = Slot { loc, meta };
        self.globals.insert(name.to_string(), slot);
        slot
    }

    /// Fails for constructs of later milestones, before anything runs (Hana
    /// validates classes before running, so a partial run would differ).
    fn check_supported(&self, stmts: &[Stmt]) -> Result<(), Unsupported> {
        let problem = std::cell::Cell::new(None);
        let note = |what: Option<&'static str>| {
            if problem.get().is_none() {
                problem.set(what);
            }
        };
        walk_stmts(stmts, &mut |s| {
            note(match s {
                Stmt::Class { .. } | Stmt::Interface { .. } | Stmt::Constructor { .. } => Some("classes"),
                Stmt::InterfaceMethod(_) => Some("interfaces"),
                Stmt::Import { .. } => Some("imports"),
                Stmt::Try { .. } | Stmt::Throw(_) => Some("try/throw"),
                Stmt::VarDecl(v) if v.is_static || v.getter.is_some() || v.setter.is_some() => Some("properties"),
                _ => None,
            })
        }, &mut |e| {
            note(match e {
                Expr::SelfRef | Expr::SuperRef | Expr::StaticRef | Expr::New { .. } => Some("classes"),
                Expr::FunctionRef(n) if n.starts_with('\'') || n.starts_with('『') => Some("reflection"),
                Expr::Binary { op, .. } if op == "instanceof" => Some("classes"),
                _ => None,
            })
        });
        match problem.get() {
            Some(p) => Err(Unsupported(p.to_string())),
            None => Ok(()),
        }
    }
}

/// Calls `s` for every statement and `e` for every expression, depth first.
fn walk_stmts(stmts: &[Stmt], s: &mut dyn FnMut(&Stmt), e: &mut dyn FnMut(&Expr)) {
    for st in stmts {
        walk_stmt(st, s, e);
    }
}

fn walk_block(b: &Block, s: &mut dyn FnMut(&Stmt), e: &mut dyn FnMut(&Expr)) {
    walk_stmts(&b.statements, s, e);
}

fn walk_stmt(st: &Stmt, s: &mut dyn FnMut(&Stmt), e: &mut dyn FnMut(&Expr)) {
    s(st);
    match st {
        Stmt::VarDecl(v) => {
            if let Some(x) = &v.value {
                walk_expr(x, e);
            }
        }
        Stmt::Assign { target, value } => {
            walk_expr(target, e);
            if let Some(x) = value {
                walk_expr(x, e);
            }
        }
        Stmt::Print { value, .. } | Stmt::Expr(value) | Stmt::Throw(value) => walk_expr(value, e),
        Stmt::Switch { discriminant, cases } => {
            walk_expr(discriminant, e);
            for c in cases {
                for t in &c.tests {
                    walk_expr(t, e);
                }
                walk_block(&c.consequent, s, e);
            }
        }
        Stmt::If { condition, consequent, alternate } => {
            walk_expr(condition, e);
            walk_block(consequent, s, e);
            if let Some(b) = alternate {
                walk_block(b, s, e);
            }
        }
        Stmt::Return(Some(x)) => walk_expr(x, e),
        Stmt::ForEach { list, body } => {
            walk_expr(list, e);
            walk_block(body, s, e);
        }
        Stmt::ForRange { start, end, body, .. } => {
            walk_expr(start, e);
            walk_expr(end, e);
            walk_block(body, s, e);
        }
        Stmt::While { condition, body } => {
            walk_expr(condition, e);
            walk_block(body, s, e);
        }
        Stmt::ListPush { target, value, .. } => {
            walk_expr(target, e);
            walk_expr(value, e);
        }
        Stmt::ListPop { target, .. } => walk_expr(target, e),
        Stmt::Function(f) => {
            for p in &f.params {
                if let Some(d) = &p.default {
                    walk_expr(d, e);
                }
            }
            walk_block(&f.body, s, e);
        }
        Stmt::Class { body, .. } | Stmt::Interface { body, .. } | Stmt::Constructor { body, .. } => walk_stmts(body, s, e),
        Stmt::Try { block, handlers, finalizer } => {
            walk_block(block, s, e);
            for h in handlers {
                walk_block(&h.body, s, e);
            }
            if let Some(f) = finalizer {
                walk_block(f, s, e);
            }
        }
        _ => {}
    }
}

fn walk_expr(x: &Expr, e: &mut dyn FnMut(&Expr)) {
    e(x);
    match x {
        Expr::Member { object, property } => {
            walk_expr(object, e);
            walk_expr(property, e);
        }
        Expr::Call { callee, args } => {
            walk_expr(callee, e);
            for a in args {
                walk_expr(a, e);
            }
        }
        Expr::New { args, .. } | Expr::List(args) => {
            for a in args {
                walk_expr(a, e);
            }
        }
        Expr::Binary { left, right, .. } | Expr::Logical { left, right, .. } => {
            walk_expr(left, e);
            walk_expr(right, e);
        }
        Expr::Dict(props) => {
            for (k, v) in props {
                walk_expr(k, e);
                walk_expr(v, e);
            }
        }
        Expr::ListPop { target, .. } => walk_expr(target, e),
        _ => {}
    }
}

/// The names a scope's statements can declare, with whether any declaration
/// of the name has a type or is a constant (it then needs a meta slot).
/// Blocks of `만약`/`따라 나누자` belong to the scope; loops, functions and
/// catch handlers open their own.
fn declared_names(stmts: &[Stmt]) -> Vec<(String, bool)> {
    fn go(stmts: &[Stmt], out: &mut Vec<(String, bool)>) {
        for s in stmts {
            match s {
                Stmt::VarDecl(v) if !v.is_static => {
                    if let Some(n) = &v.name {
                        add(out, n, v.type_ref.is_some() || v.is_constant);
                    }
                }
                Stmt::Input { target: Some(n), .. } => add(out, n, false),
                Stmt::If { consequent, alternate, .. } => {
                    go(&consequent.statements, out);
                    if let Some(b) = alternate {
                        go(&b.statements, out);
                    }
                }
                Stmt::Switch { cases, .. } => {
                    for c in cases {
                        go(&c.consequent.statements, out);
                    }
                }
                Stmt::Try { block, finalizer, .. } => {
                    go(&block.statements, out);
                    if let Some(f) = finalizer {
                        go(&f.statements, out);
                    }
                }
                _ => {}
            }
        }
    }
    fn add(out: &mut Vec<(String, bool)>, name: &str, meta: bool) {
        match out.iter_mut().find(|(n, _)| n == name) {
            Some(e) => e.1 |= meta,
            None => out.push((name.to_string(), meta)),
        }
    }
    let mut out = Vec::new();
    go(stmts, &mut out);
    out
}

struct Scope {
    names: HashMap<String, Slot>,
}

struct LoopCtx {
    breaks: Vec<usize>,
}

struct FnCompiler<'c> {
    c: &'c mut Compiler,
    code: Vec<Op>,
    next_reg: Reg,
    max_reg: Reg,
    /// Scopes of this function, outermost first (the top level's scope is
    /// the globals and is not listed here).
    scopes: Vec<Scope>,
    loops: Vec<LoopCtx>,
    loop_ranges: Vec<LoopRange>,
    handlers: Vec<Handler>,
    open_calls: u32,
    /// Variable registers that certainly hold a value wherever their scope is
    /// visible: parameters (after the prologue), loop variables, items.
    definite: Vec<Reg>,
}

impl<'c> FnCompiler<'c> {
    fn new(c: &'c mut Compiler, _f: Option<&ast::FuncDecl>) -> FnCompiler<'c> {
        FnCompiler {
            c,
            code: Vec::new(),
            next_reg: 0,
            max_reg: 0,
            scopes: Vec::new(),
            loops: Vec::new(),
            loop_ranges: Vec::new(),
            handlers: Vec::new(),
            open_calls: 0,
            definite: Vec::new(),
        }
    }

    fn finish(self, name: &str, params: Vec<Param>, return_type: u32) -> Proto {
        Proto {
            name: name.to_string(),
            code: self.code,
            nregs: self.max_reg,
            params,
            return_type,
            loops: self.loop_ranges,
            handlers: self.handlers,
        }
    }

    fn compile_main(mut self, stmts: &[Stmt]) -> Proto {
        self.stmts(stmts);
        self.emit(Op::ReturnNull);
        self.finish("<main>", Vec::new(), 0)
    }

    fn compile_function(mut self, f: &ast::FuncDecl) -> Proto {
        // Parameters take the first registers: arguments arrive there.
        let mut scope = Scope { names: HashMap::new() };
        let mut params = Vec::new();
        let first_regs: Vec<Reg> = f.params.iter().map(|_| self.alloc()).collect();
        for (i, p) in f.params.iter().enumerate() {
            let ty = p.type_annotation.as_ref().map_or(0, |t| self.c.type_id(&t.name));
            let meta = (ty != 0).then(|| self.alloc());
            let slot = Slot { loc: Loc::Reg(first_regs[i]), meta: meta.map(Loc::Reg) };
            scope.names.insert(p.name.clone(), slot);
            params.push(Param { name: self.c.name(&p.name), slot: first_regs[i], meta, ty });
        }
        // A repeated parameter name is one variable: the later parameter's.
        for (name, flags) in declared_names(&f.body.statements) {
            match scope.names.get_mut(&name) {
                Some(slot) => {
                    if flags && slot.meta.is_none() {
                        slot.meta = Some(Loc::Reg(self.alloc()));
                    }
                }
                None => {
                    let loc = Loc::Reg(self.alloc());
                    let meta = flags.then(|| Loc::Reg(self.alloc()));
                    scope.names.insert(name, Slot { loc, meta });
                }
            }
        }
        // Parameter metas must point at the scope's final slots.
        for p in params.iter_mut() {
            let name = self.c.prog.names[p.name as usize].clone();
            let slot = scope.names[&name];
            p.meta = slot.meta.map(|m| match m {
                Loc::Reg(r) => r,
                Loc::Global(_) => unreachable!(),
            });
        }
        self.scopes.push(scope);

        // Missing arguments: defaults (evaluated in the call's scope) or an error.
        for (i, p) in f.params.iter().enumerate() {
            let at = self.emit(Op::ArgGiven { index: i as u16, skip: 0 });
            match &p.default {
                Some(d) => {
                    let mark = self.next_reg;
                    let r = self.alloc();
                    self.expr_to(d, r);
                    self.emit(Op::BindParam { index: i as u16, src: r });
                    self.next_reg = mark;
                }
                None => {
                    self.emit(Op::MissingArg { index: i as u16 });
                }
            }
            let here = self.here();
            self.patch_jump(at, here);
        }

        // A parameter's slot is defined from here on, unless another name's
        // declaration shares it (a repeated parameter name).
        for p in &params {
            let name = &self.c.prog.names[p.name as usize];
            if let Some(Slot { loc: Loc::Reg(r), .. }) = self.scopes[0].names.get(name.as_str()).copied() {
                if r == p.slot {
                    self.definite.push(r);
                }
            }
        }
        self.stmts(&f.body.statements);
        self.emit(Op::ReturnNull);
        let return_type = f.return_type.as_ref().map_or(0, |t| self.c.type_id(&t.name));
        self.finish(&f.name, params, return_type)
    }

    // ---- registers and code

    fn alloc(&mut self) -> Reg {
        let r = self.next_reg;
        self.next_reg += 1;
        self.max_reg = self.max_reg.max(self.next_reg);
        r
    }

    fn emit(&mut self, op: Op) -> usize {
        self.code.push(op);
        self.code.len() - 1
    }

    fn here(&self) -> u32 {
        self.code.len() as u32
    }

    fn patch_jump(&mut self, at: usize, target: u32) {
        match &mut self.code[at] {
            Op::Jump { to } | Op::JumpIfFalse { to, .. } | Op::JumpIfTrue { to, .. } => *to = target,
            Op::RangeTest { exit, .. } | Op::IterNext { exit, .. } => *exit = target,
            Op::ArgGiven { skip, .. } | Op::Member { skip, .. } | Op::SetMember { skip, .. } => *skip = target,
            Op::SetIndexFail { skip, .. } => *skip = target,
            other => panic!("not a jump: {other:?}"),
        }
    }

    fn unsupported(&mut self, what: &str) {
        let k = self.c.str_const(what);
        self.emit(Op::Unsupported { k });
    }

    // ---- variables

    /// The slots `name` may be in from here, innermost first.
    fn var(&mut self, name: &str) -> u32 {
        let mut slots: Vec<Slot> = self.scopes.iter().rev().filter_map(|s| s.names.get(name).copied()).collect();
        if let Some(g) = self.c.globals.get(name) {
            slots.push(*g);
        }
        let v = Var { name: self.c.name(name), slots };
        self.c.prog.vars.push(v);
        (self.c.prog.vars.len() - 1) as u32
    }

    fn get_var(&mut self, name: &str, dst: Reg) {
        let var = self.var(name);
        let v = &self.c.prog.vars[var as usize];
        let op = match v.slots.as_slice() {
            [s] => match s.loc {
                Loc::Reg(slot) => Op::GetReg { dst, slot, name: v.name },
                Loc::Global(slot) => Op::GetGlobal { dst, slot, name: v.name },
            },
            _ => Op::GetVar { dst, var },
        };
        self.emit(op);
    }

    /// Opens a scope with slots for `names` (plus `extra`, declared by the
    /// construct itself), in registers `from..to`.
    fn open_scope(&mut self, extra: &[&str], stmts: &[Stmt]) -> (Reg, Reg) {
        let from = self.next_reg;
        let mut scope = Scope { names: HashMap::new() };
        for name in extra {
            let loc = Loc::Reg(self.alloc());
            scope.names.insert(name.to_string(), Slot { loc, meta: None });
        }
        for (name, flags) in declared_names(stmts) {
            match scope.names.get_mut(&name) {
                Some(slot) => {
                    if flags && slot.meta.is_none() {
                        slot.meta = Some(Loc::Reg(self.alloc()));
                    }
                }
                None => {
                    let loc = Loc::Reg(self.alloc());
                    let meta = flags.then(|| Loc::Reg(self.alloc()));
                    scope.names.insert(name, Slot { loc, meta });
                }
            }
        }
        self.scopes.push(scope);
        (from, self.next_reg)
    }

    fn close_scope(&mut self, from: Reg) {
        self.scopes.pop();
        self.next_reg = from;
        self.definite.retain(|&r| r < from);
    }

    fn slot_reg(&self, name: &str) -> Reg {
        match self.scopes.last().unwrap().names[name].loc {
            Loc::Reg(r) => r,
            Loc::Global(_) => unreachable!(),
        }
    }

    // ---- statements

    fn stmts(&mut self, stmts: &[Stmt]) {
        for s in stmts {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        let mark = self.next_reg;
        match s {
            Stmt::VarDecl(v) => {
                let name = v.name.as_deref().unwrap_or("");
                let r = self.alloc();
                match &v.value {
                    Some(e) => self.expr_to(e, r),
                    None => {
                        self.emit(Op::LoadNull { dst: r });
                    }
                }
                let ty = v.type_ref.as_ref().map_or(0, |t| self.c.type_id(&t.name));
                self.decl(name, r, ty, v.is_constant);
            }
            Stmt::Assign { target, value } => self.assign(target, value.as_ref()),
            Stmt::Print { value, newline } => {
                let r = self.alloc();
                self.expr_to(value, r);
                self.emit(Op::Print { src: r, newline: *newline });
            }
            Stmt::Input { target, type_ref } => {
                let ty = type_ref.as_ref().map_or(0, |t| self.c.type_id(&t.name));
                let var = match target {
                    Some(n) => self.var(n),
                    None => NONE,
                };
                self.emit(Op::Input { var, ty });
            }
            Stmt::Expr(e) => {
                let r = self.alloc();
                self.expr_to(e, r);
            }
            Stmt::If { condition, consequent, alternate } => {
                let r = self.alloc();
                self.expr_to(condition, r);
                let jf = self.emit(Op::JumpIfFalse { cond: r, to: 0 });
                self.next_reg = mark;
                self.stmts(&consequent.statements);
                match alternate {
                    Some(alt) => {
                        let j = self.emit(Op::Jump { to: 0 });
                        let here = self.here();
                        self.patch_jump(jf, here);
                        self.stmts(&alt.statements);
                        let here = self.here();
                        self.patch_jump(j, here);
                    }
                    None => {
                        let here = self.here();
                        self.patch_jump(jf, here);
                    }
                }
            }
            Stmt::Switch { discriminant, cases } => self.switch(discriminant, cases),
            Stmt::Fallthrough => {} // only meaningful directly in a case body
            Stmt::Return(value) => {
                let r = self.alloc();
                match value {
                    Some(e) => self.expr_to(e, r),
                    None => {
                        self.emit(Op::LoadNull { dst: r });
                    }
                }
                self.emit(Op::Return { src: r });
            }
            Stmt::Break => match self.loops.last_mut() {
                Some(_) => {
                    let at = self.emit(Op::Jump { to: 0 });
                    self.loops.last_mut().unwrap().breaks.push(at);
                }
                None => {
                    self.emit(Op::Break);
                }
            },
            Stmt::ForRange { start, end, loop_var, body } => self.for_range(start, end, loop_var, body),
            Stmt::ForEach { list, body } => self.for_each(list, body),
            Stmt::While { condition, body } => self.while_loop(condition, body),
            Stmt::ListPush { target, value, position } => {
                let list = self.alloc();
                self.expr_to(target, list);
                let target_var = self.target_var(target);
                self.emit(Op::ListCheck { list, target: target_var });
                let val = self.alloc();
                self.expr_to(value, val);
                self.emit(Op::ListPush { list, val, front: *position == "front", target: target_var });
            }
            Stmt::ListPop { target, position } => {
                let list = self.alloc();
                self.expr_to(target, list);
                let target_var = self.target_var(target);
                self.emit(Op::ListCheck { list, target: target_var });
                self.emit(Op::ListPop { dst: list, list, front: *position == "front" });
            }
            // A function runs only when called; a declaration elsewhere than
            // the top level is never found (Hana ignores it).
            Stmt::Function(_) => {}
            _ => self.unsupported("statement"),
        }
        self.next_reg = mark;
    }

    /// The variable a push/pop/clear target names (for the constant and type
    /// checks), or NONE.
    fn target_var(&mut self, target: &Expr) -> u32 {
        match target {
            Expr::Identifier(n) => self.var(n),
            _ => NONE,
        }
    }

    fn decl(&mut self, name: &str, src: Reg, ty: u32, konst: bool) {
        let var = self.var(name);
        let v = &self.c.prog.vars[var as usize];
        // One possible slot, nothing typed or constant: a plain store.
        if let [Slot { loc, meta: None }] = v.slots.as_slice() {
            if ty == 0 && !konst {
                let op = match *loc {
                    Loc::Reg(slot) => Op::SetReg { slot, src },
                    Loc::Global(slot) => Op::SetGlobal { slot, src },
                };
                self.emit(op);
                return;
            }
        }
        self.emit(Op::Decl { var, src, ty, konst });
    }

    fn assign(&mut self, target: &Expr, value: Option<&Expr>) {
        // `'x'에 y를 더하자` is `x = x + y`: one instruction, so a string can grow in place.
        if let (Expr::Identifier(n), Some(Expr::Binary { left, op, right })) = (target, value) {
            if matches!(&**left, Expr::Identifier(m) if m == n) && (op == "+" || op == "-") {
                let a = self.alloc();
                self.get_var(n, a);
                let b = self.alloc();
                self.expr_to(right, b);
                let var = self.var(n);
                let op = if op == "+" { BinOp::Add } else { BinOp::Sub };
                self.emit(Op::Update { var, a, b, op });
                return;
            }
        }
        // Hana evaluates the value first, then the target.
        let val = self.alloc();
        match value {
            Some(e) => self.expr_to(e, val),
            None => {
                self.emit(Op::LoadNull { dst: val });
            }
        }
        match target {
            Expr::Identifier(n) => {
                let var = self.var(n);
                self.emit(Op::Assign { var, src: val });
            }
            Expr::Member { object, property } => {
                let obj = self.alloc();
                self.expr_to(object, obj);
                let set = self.emit(Op::SetMember { obj, skip: 0 });
                let key = self.alloc();
                let name = match &**property {
                    Expr::Identifier(n) => self.c.name(n),
                    _ => NONE,
                };
                let start = self.here();
                self.expr_to(property, key);
                let end = self.here();
                self.emit(Op::SetIndex { obj, key, val });
                let done = self.emit(Op::Jump { to: 0 });
                let target = self.here();
                let fail = self.emit(Op::SetIndexFail { obj, val, name, skip: 0 });
                self.handlers.push(Handler { start, end, target, open_calls: self.open_calls });
                let here = self.here();
                self.patch_jump(set, here);
                self.patch_jump(done, here);
                self.patch_jump(fail, here);
            }
            _ => {} // Hana ignores other targets
        }
    }

    fn switch(&mut self, discriminant: &Expr, cases: &[ast::SwitchCase]) {
        let d = self.alloc();
        self.expr_to(discriminant, d);
        let t = self.alloc();
        let mut to_body: Vec<Vec<usize>> = Vec::new(); // jumps into each body
        let mut to_next_test: Option<usize> = None;
        let mut bodies = Vec::new();
        // Tests, in order; a default matches when reached.
        for c in cases {
            if let Some(j) = to_next_test.take() {
                let here = self.here();
                self.patch_jump(j, here);
            }
            let mut jumps = Vec::new();
            if c.is_default {
                jumps.push(self.emit(Op::Jump { to: 0 }));
            } else {
                for test in &c.tests {
                    self.expr_to(test, t);
                    self.emit(Op::Eq { dst: t, a: d, b: t, neg: false });
                    jumps.push(self.emit(Op::JumpIfTrue { cond: t, to: 0 }));
                }
                to_next_test = Some(self.emit(Op::Jump { to: 0 }));
            }
            to_body.push(jumps);
        }
        let mut to_end: Vec<usize> = to_next_test.into_iter().collect();
        let mut fall_from: Option<usize> = None;
        for (i, c) in cases.iter().enumerate() {
            let start = self.here();
            for j in &to_body[i] {
                self.patch_jump(*j, start);
            }
            if let Some(j) = fall_from.take() {
                self.patch_jump(j, start);
            }
            // Statements up to a direct `다음으로 이어가자`, which goes on
            // into the next case's body.
            let mut fell = false;
            for s in &c.consequent.statements {
                if matches!(s, Stmt::Fallthrough) {
                    fell = true;
                    break;
                }
                self.stmt(s);
            }
            let j = self.emit(Op::Jump { to: 0 });
            if fell {
                fall_from = Some(j);
            } else {
                to_end.push(j);
            }
            bodies.push(start);
        }
        let end = self.here();
        for j in to_end.into_iter().chain(fall_from) {
            self.patch_jump(j, end);
        }
    }

    fn loop_body(&mut self, body: &Block, top: u32) -> Vec<usize> {
        self.loops.push(LoopCtx { breaks: Vec::new() });
        self.stmts(&body.statements);
        self.emit(Op::Jump { to: top });
        self.loops.pop().unwrap().breaks
    }

    fn end_loop(&mut self, start: u32, exits: Vec<usize>) {
        let exit = self.here();
        for j in exits {
            self.patch_jump(j, exit);
        }
        self.loop_ranges.push(LoopRange { start, end: exit, exit });
    }

    fn for_range(&mut self, start: &Expr, end: &Expr, loop_var: &str, body: &Block) {
        let v = self.alloc();
        self.expr_to(start, v);
        let e = self.alloc();
        self.expr_to(end, e);
        let step = self.alloc();
        self.emit(Op::RangePrep { start: v, end: e, step });
        let name = if loop_var.is_empty() { self.c.prog.lang.default_index } else { loop_var };
        let (from, to) = self.open_scope(&[name], &body.statements);
        let var_slot = self.slot_reg(name);
        self.definite.push(var_slot);
        let top = self.here();
        let test = self.emit(Op::RangeTest { v, end: e, step, exit: 0 });
        self.emit(Op::Undef { from, to });
        self.emit(Op::Move { dst: var_slot, src: v });
        self.emit(Op::Boxed { dst: var_slot });
        let step_at = self.here();
        let mut exits = self.loop_body(body, top);
        // The step goes before the jump back: rewrite the jump into step + jump.
        let back = self.code.pop();
        self.emit(Op::RangeStep { v, step });
        self.code.push(back.unwrap());
        let _ = step_at;
        exits.push(test);
        self.close_scope(from);
        self.end_loop(top, exits);
    }

    fn for_each(&mut self, list: &Expr, body: &Block) {
        // `'목록'의 '항목'마다`: the member expression names the list and the item.
        let lang = self.c.prog.lang;
        let (iterable, item) = match list {
            Expr::Member { object, property } => match &**property {
                Expr::Identifier(n) => (&**object, n.as_str()),
                _ => (&**object, lang.default_item),
            },
            other => (other, lang.default_item),
        };
        let src = self.alloc();
        self.expr_to(iterable, src);
        let iter = self.alloc();
        self.emit(Op::IterPrep { dst: iter, src });
        let idx = self.alloc();
        let zero = self.c.num_const(0.0);
        self.emit(Op::LoadK { dst: idx, k: zero });
        let (from, to) = self.open_scope(&[item], &body.statements);
        let item_slot = self.slot_reg(item);
        self.definite.push(item_slot);
        let top = self.here();
        self.emit(Op::Undef { from, to });
        let next = self.emit(Op::IterNext { iter, idx, dst: item_slot, exit: 0 });
        let mut exits = self.loop_body(body, top);
        exits.push(next);
        self.close_scope(from);
        self.end_loop(top, exits);
    }

    fn while_loop(&mut self, condition: &Expr, body: &Block) {
        let top = self.here();
        let c = self.alloc();
        self.expr_to(condition, c);
        let jf = self.emit(Op::JumpIfFalse { cond: c, to: 0 });
        let (from, to) = self.open_scope(&[], &body.statements);
        if to > from {
            self.emit(Op::Undef { from, to });
        }
        let mut exits = self.loop_body(body, top);
        exits.push(jf);
        self.close_scope(from);
        self.end_loop(top, exits);
    }

    // ---- expressions

    fn expr_to(&mut self, e: &Expr, dst: Reg) {
        let mark = self.next_reg;
        match e {
            Expr::Number(n) => {
                // Hana boxes literals with `num.Box`: -0 reads as 0.
                let k = self.c.num_const(if *n == 0.0 { 0.0 } else { *n });
                self.emit(Op::LoadK { dst, k });
            }
            Expr::Str(raw) => {
                let k = self.c.str_const(&unescape(raw));
                self.emit(Op::LoadK { dst, k });
            }
            Expr::Bool(b) => {
                self.emit(Op::LoadBool { dst, v: *b });
            }
            Expr::Null => {
                self.emit(Op::LoadNull { dst });
            }
            Expr::Identifier(n) => self.get_var(n, dst),
            // A type used as a value: its name (classes come later).
            Expr::TypeRef(t) => {
                let k = self.c.str_const(&t.name);
                self.emit(Op::LoadK { dst, k });
            }
            Expr::FunctionRef(n) => {
                let var = self.var(n);
                let name = self.c.name(n);
                self.emit(Op::FuncRef { dst, name, var });
            }
            Expr::Template(raw) => self.template(raw, dst),
            Expr::List(items) => {
                let base = self.next_reg;
                for item in items {
                    let r = self.alloc();
                    self.expr_to(item, r);
                }
                self.emit(Op::MakeList { dst, base, n: items.len() as u16 });
            }
            Expr::Dict(props) => {
                let base = self.next_reg;
                for (k, v) in props {
                    let rk = self.alloc();
                    self.expr_to(k, rk);
                    let rv = self.alloc();
                    self.expr_to(v, rv);
                }
                self.emit(Op::MakeDict { dst, base, n: props.len() as u16 });
            }
            Expr::Binary { left, op, right } => self.binary(left, op, right, dst),
            Expr::Logical { left, op, right } => {
                self.expr_to(left, dst);
                self.emit(Op::Truth { dst, src: dst });
                let j = if op == "그리고" {
                    self.emit(Op::JumpIfFalse { cond: dst, to: 0 })
                } else {
                    self.emit(Op::JumpIfTrue { cond: dst, to: 0 })
                };
                self.expr_to(right, dst);
                self.emit(Op::Truth { dst, src: dst });
                let here = self.here();
                self.patch_jump(j, here);
            }
            Expr::Member { object, property } => self.member(object, property, dst),
            Expr::Call { callee, args } => self.call(callee, args, dst),
            Expr::ListPop { target, position } => {
                self.expr_to(target, dst);
                let target_var = self.target_var(target);
                self.emit(Op::ListCheck { list: dst, target: target_var });
                self.emit(Op::ListPop { dst, list: dst, front: *position == "front" });
            }
            _ => self.unsupported("expression"),
        }
        self.next_reg = mark;
    }

    fn binary(&mut self, left: &Expr, op: &str, right: &Expr, dst: Reg) {
        let a = self.operand(left);
        let bin = |op| Some(op);
        let arith = match op {
            "+" => bin(BinOp::Add),
            "-" => bin(BinOp::Sub),
            "*" => bin(BinOp::Mul),
            "/" => bin(BinOp::Div),
            "%" => bin(BinOp::Mod),
            ">" => bin(BinOp::Gt),
            "<" => bin(BinOp::Lt),
            ">=" => bin(BinOp::Ge),
            "<=" => bin(BinOp::Le),
            _ => None,
        };
        if let (Some(op), Expr::Number(n)) = (arith, right) {
            let k = self.c.num_const(if *n == 0.0 { 0.0 } else { *n });
            self.emit(Op::BinK { op, dst, a, k });
            return;
        }
        let b = self.operand(right);
        let kind = match op {
            "+" => bin(BinOp::Add),
            "-" => bin(BinOp::Sub),
            "*" => bin(BinOp::Mul),
            "/" => bin(BinOp::Div),
            "%" => bin(BinOp::Mod),
            ">" => bin(BinOp::Gt),
            "<" => bin(BinOp::Lt),
            ">=" => bin(BinOp::Ge),
            "<=" => bin(BinOp::Le),
            "==" | "!=" => {
                self.emit(Op::Eq { dst, a, b, neg: op == "!=" });
                return;
            }
            _ => None,
        };
        match kind {
            Some(op) => {
                self.emit(Op::Bin { op, dst, a, b });
            }
            None => {
                let k = self.c.str_const(op);
                self.emit(Op::UnknownOp { k });
            }
        }
    }

    /// A register holding the value of `e`: the variable's own slot when it
    /// is certainly defined here (a parameter, a loop's variable), else a
    /// new register it is computed into. For reading only.
    fn operand(&mut self, e: &Expr) -> Reg {
        if let Expr::Identifier(n) = e {
            let first = self.scopes.iter().rev().find_map(|s| s.names.get(n.as_str()).copied());
            if let Some(Slot { loc: Loc::Reg(r), .. }) = first {
                if self.definite.contains(&r) {
                    return r;
                }
            }
        }
        let r = self.alloc();
        self.expr_to(e, r);
        r
    }

    fn member(&mut self, object: &Expr, property: &Expr, dst: Reg) {
        let obj = self.alloc();
        self.expr_to(object, obj);
        let name = match property {
            Expr::Identifier(n) => self.c.name(n),
            Expr::FunctionRef(_) => {
                // A bound method as a value (not called): later.
                self.unsupported("method value");
                return;
            }
            _ => NONE,
        };
        let pre = self.emit(Op::Member { dst, obj, name, skip: 0 });
        let key = self.alloc();
        let start = self.here();
        self.expr_to(property, key);
        let end = self.here();
        self.emit(Op::Index { dst, obj, key });
        let done = self.emit(Op::Jump { to: 0 });
        let target = self.here();
        self.emit(Op::IndexFail { obj });
        self.handlers.push(Handler { start, end, target, open_calls: self.open_calls });
        let here = self.here();
        self.patch_jump(pre, here);
        self.patch_jump(done, here);
    }

    fn args(&mut self, args: &[Expr]) -> Reg {
        let base = self.next_reg;
        for a in args {
            let r = self.alloc();
            self.expr_to(a, r);
        }
        base
    }

    fn call(&mut self, callee: &Expr, args: &[Expr], dst: Reg) {
        self.emit(Op::Enter);
        self.open_calls += 1;
        let argc = args.len() as u16;
        match callee {
            Expr::FunctionRef(name) => self.call_named(name, args, dst),
            // `TYPE의 〈함수〉()` where TYPE is not a class: a plain call.
            Expr::Member { object, property } if matches!(&**object, Expr::TypeRef(_)) && matches!(&**property, Expr::FunctionRef(_)) => {
                let Expr::FunctionRef(name) = &**property else { unreachable!() };
                self.call_named(name, args, dst);
            }
            Expr::Member { object, property } if matches!(&**property, Expr::FunctionRef(_)) => {
                let Expr::FunctionRef(name) = &**property else { unreachable!() };
                let obj = self.alloc();
                self.expr_to(object, obj);
                let name = self.c.name(name);
                let target = self.target_var(object);
                let base = self.args(args);
                self.emit(Op::CallMethod { dst, obj, name, target, base, argc });
            }
            other => {
                let callee = self.alloc();
                self.expr_to(other, callee);
                let base = self.args(args);
                self.emit(Op::CallValue { dst, callee, base, argc });
            }
        }
        self.open_calls -= 1;
    }

    fn call_named(&mut self, name: &str, args: &[Expr], dst: Reg) {
        let argc = args.len() as u16;
        if let Some(&proto) = self.c.functions.get(name) {
            let base = self.args(args);
            self.emit(Op::Call { dst, proto, base, argc });
        } else {
            let var = self.var(name);
            let n = self.c.name(name);
            let base = self.args(args);
            self.emit(Op::CallName { dst, name: n, var, base, argc });
        }
    }

    fn template(&mut self, raw: &str, dst: Reg) {
        let base = self.next_reg;
        let mut n = 0u16;
        for part in split_template(raw) {
            let r = self.alloc();
            n += 1;
            match part {
                TemplatePart::Text(t) => {
                    let k = self.c.str_const(&t);
                    self.emit(Op::LoadK { dst: r, k });
                }
                TemplatePart::Code(code) => {
                    let e = parse_embedded(&code, self.c.prog.lang);
                    self.expr_to(&e, r);
                    self.emit(Op::Format { dst: r, src: r });
                }
            }
        }
        self.emit(Op::Concat { dst, base, n });
    }
}

/// A string literal's escapes, the way Hana replaces them (in this order).
pub fn unescape(s: &str) -> String {
    s.replace("\\n", "\n").replace("\\\"", "\"").replace("\\t", "\t").replace("\\\\", "\\")
}

enum TemplatePart {
    Text(String),
    Code(String),
}

/// Splits a template after unescaping: text, then `{code}`, and so on. An
/// unclosed `{` is text (Hana's `parseTemplate`).
fn split_template(raw: &str) -> Vec<TemplatePart> {
    let mut raw = unescape(raw);
    let mut parts = Vec::new();
    while !raw.is_empty() {
        let Some(open) = raw.find('{') else {
            parts.push(TemplatePart::Text(raw));
            break;
        };
        let text = raw[..open].to_string();
        let rest = raw[open + 1..].to_string();
        let Some(close) = rest.find('}') else {
            parts.push(TemplatePart::Text(format!("{text}{{{rest}")));
            break;
        };
        if !text.is_empty() {
            parts.push(TemplatePart::Text(text));
        }
        parts.push(TemplatePart::Code(rest[..close].to_string()));
        raw = rest[close + 1..].to_string();
    }
    parts
}

/// An expression inside a template, read by the template's own language.
fn parse_embedded(code: &str, lang: &Lang) -> Expr {
    let tokens = haru_syntax::lexer::tokenize(code, lang.syntax);
    haru_syntax::Parser::new(tokens, lang.syntax).parse_expression()
}
