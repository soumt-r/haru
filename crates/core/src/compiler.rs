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

use std::collections::{HashMap, HashSet};

use haru_syntax::ast::{self, Block, Expr, Stmt};

use crate::bytecode::*;
use crate::lang::Lang;
use crate::symbol;
use crate::value::{FuncObj, Value};

/// A construct this version cannot run yet.
#[derive(Debug)]
pub struct Unsupported(pub String);

/// What a `[모듈]` name means to this build, in a language: `None` when no
/// standard module has that name; `Some((true, functions))` for one Haru has
/// (its functions' names in that language); `Some((false, _))` for one only
/// Hana has so far.
pub type StdLookup<'s> = &'s dyn Fn(&str, &str) -> Option<(bool, Vec<String>)>;

/// A file module read and parsed ahead (import paths are written literally).
enum Source {
    Ok(ast::Program, &'static Lang),
    /// The error its import raises when it runs: (code, argument).
    Fail(&'static str, String),
}

/// Every file the statements import, and the files those import, by key.
fn discover(stmts: &[Stmt], sources: &mut HashMap<String, Source>) {
    let mut found = Vec::new();
    walk_stmts(
        stmts,
        &mut |s| {
            if let Stmt::Import { module, is_builtin: false, .. } = s {
                found.push(module.clone());
            }
        },
        &mut |_| {},
    );
    for path in found {
        let key = format!("file:{path}");
        if sources.contains_key(&key) {
            continue;
        }
        let lang = crate::lang::for_path(std::path::Path::new(&path));
        let source = match std::fs::read_to_string(&path) {
            Err(_) => Source::Fail("ImportError.ImportFileNotFound", path.clone()),
            Ok(text) => match haru_syntax::parse(&text, lang.syntax) {
                (prog, diags) if diags.is_empty() => Source::Ok(prog, lang),
                _ => Source::Fail("ImportError.ImportFileSyntax", path.clone()),
            },
        };
        sources.insert(key.clone(), source);
        if let Source::Ok(prog, _) = &sources[&key] {
            // Borrow ends before the recursive insertions.
            let stmts: Vec<Stmt> = prog.statements.clone();
            discover(&stmts, sources);
        }
    }
}

pub fn compile(program: &ast::Program, lang: &'static Lang, std_lookup: StdLookup) -> Result<Program, Unsupported> {
    let mut sources = HashMap::new();
    discover(&program.statements, &mut sources);
    check_supported(&program.statements, lang, std_lookup)?;
    for s in sources.values() {
        if let Source::Ok(p, l) = s {
            check_supported(&p.statements, l, std_lookup)?;
        }
    }
    let error_classes = [builtin_error_class(&crate::lang::HARI), builtin_error_class(&crate::lang::KANADE)];

    let mut c = Compiler {
        prog: Program {
            lang,
            protos: Vec::new(),
            consts: Vec::new(),
            vars: Vec::new(),
            types: vec![TypeSpec::parse("", lang)],
            globals: Vec::new(),
            modules: Vec::new(),
            classes: Vec::new(),
            imports: Vec::new(),
        },
        type_ids: HashMap::new(),
        str_consts: HashMap::new(),
        sources: &sources,
        std_lookup,
        module_ids: HashMap::new(),
        lang,
        module: 0,
        globals: HashMap::new(),
        functions: HashMap::new(),
        class_defs: HashMap::new(),
        class_ids: HashMap::new(),
    };
    // Module ids first (imports refer to them), then each module's code.
    c.module_ids.insert("<main>".to_string(), 0);
    let mut keys: Vec<&String> = sources.iter().filter(|(_, s)| matches!(s, Source::Ok(..))).map(|(k, _)| k).collect();
    keys.sort();
    for (i, k) in keys.iter().enumerate() {
        c.module_ids.insert((*k).clone(), (i + 1) as u32);
    }
    c.compile_module("<main>", &program.statements, lang, &error_classes);
    for k in keys {
        let Source::Ok(p, l) = &sources[k] else { unreachable!() };
        c.compile_module(k, &p.statements, l, &error_classes);
    }
    Ok(c.prog)
}

impl<'a> Compiler<'a> {
    /// Compiles one module (the program or an imported file) with its own
    /// globals, functions and classes.
    fn compile_module(&mut self, key: &str, stmts: &'a [Stmt], lang: &'static Lang, error_classes: &'a [Stmt; 2]) {
        let id = self.prog.modules.len() as u32;
        self.lang = lang;
        self.module = id;
        self.globals = HashMap::new();
        self.functions = HashMap::new();
        self.class_defs = HashMap::new();
        self.class_ids = HashMap::new();
        let global_start = self.prog.globals.len() as u32;

        // Built-ins are variables of the top level (a program may even replace them).
        for (i, name) in lang.builtins.iter().enumerate() {
            let slot = self.new_global(name, false);
            self.prog.globals[slot.loc_global() as usize] = Value::func(FuncObj::Builtin(i as u8));
        }
        for (name, flags) in self.declared_names(stmts) {
            if !self.globals.contains_key(&name) {
                self.new_global(&name, flags);
            } else if flags {
                let slot = self.globals.get_mut(&name).unwrap();
                if slot.meta.is_none() {
                    let meta = self.prog.globals.len() as u32;
                    self.prog.globals.push(Value::UNDEF);
                    slot.meta = Some(Loc::Global(meta));
                }
            }
        }

        // Classes and interfaces of the top level, the later of a name winning,
        // after the built-in error class (which a program may replace).
        let error_class = if lang.name == "kanade" { &error_classes[1] } else { &error_classes[0] };
        let mut class_order: Vec<String> = Vec::new();
        let mut defs: HashMap<String, &'a Stmt> = HashMap::new();
        let mut interfaces = HashSet::new();
        for s in std::iter::once(error_class).chain(stmts.iter()) {
            match s {
                Stmt::Class { name: Some(n), .. } => {
                    if defs.insert(n.name.clone(), s).is_none() {
                        class_order.push(n.name.clone());
                    }
                }
                Stmt::Interface { name: Some(n), .. } => {
                    interfaces.insert(symbol::intern(&n.name));
                }
                _ => {}
            }
        }

        // Every top-level function gets a body; calls find the first of a name.
        let base = self.prog.protos.len() as u32; // `base` is the module's top level
        let mut jobs: Vec<Job> = Vec::new();
        let mut all_functions = Vec::new();
        for s in stmts {
            if let Stmt::Function(f) = s {
                jobs.push(Job::Function(f));
                let proto = base + jobs.len() as u32;
                all_functions.push((f.name.clone(), proto));
                self.functions.entry(f.name.clone()).or_insert(proto);
            }
        }
        // Every method, constructor, getter, setter and field initializer of every class.
        let mut class_protos: HashMap<String, ClassProtos> = HashMap::new();
        for name in &class_order {
            let Stmt::Class { body, .. } = defs[name] else { unreachable!() };
            let mut cp = ClassProtos::default();
            jobs.push(Job::Init(body));
            cp.init = base + jobs.len() as u32;
            for s in body {
                match s {
                    Stmt::Function(f) => {
                        jobs.push(Job::Method(f));
                        cp.methods.push((f.name.clone(), base + jobs.len() as u32, f.is_static, Access::parse(f.access)));
                        if f.name == lang.equals_method && cp.equals.is_none() {
                            jobs.push(Job::Equals(f));
                            cp.equals = Some(base + jobs.len() as u32);
                        }
                    }
                    Stmt::Constructor { params, body, .. } => {
                        jobs.push(Job::Ctor(params, body));
                        if cp.ctor.is_none() {
                            cp.ctor = Some(base + jobs.len() as u32);
                        }
                    }
                    Stmt::VarDecl(v) => {
                        let name = v.name.clone().unwrap_or_default();
                        let getter = v.getter.as_ref().map(|g| {
                            jobs.push(Job::Getter(g));
                            base + jobs.len() as u32
                        });
                        let setter = v.setter.as_ref().map(|st| {
                            jobs.push(Job::Setter(st));
                            base + jobs.len() as u32
                        });
                        let ty = if v.is_static { None } else { Some(v.type_ref.as_ref().map_or(0, |t| self.type_id(&t.name))) };
                        cp.fields.push(FieldDecl { name, access: Access::parse(v.access), getter, setter, ty });
                    }
                    _ => {}
                }
            }
            class_protos.insert(name.clone(), cp);
        }
        self.class_defs = defs;
        self.build_classes(&class_order, &class_protos);

        self.prog.protos.push(placeholder("<main>", id));
        for _ in &jobs {
            self.prog.protos.push(placeholder("", id));
        }
        let main = FnCompiler::new(self, false).compile_main(stmts);
        self.prog.protos[base as usize] = main;
        for (i, job) in jobs.iter().enumerate() {
            let mut proto = match *job {
                Job::Function(f) => FnCompiler::new(self, false).compile_callable(
                    &f.name,
                    &f.params,
                    &f.body.statements,
                    f.return_type.as_ref(),
                    false,
                ),
                Job::Method(f) => FnCompiler::new(self, true).compile_callable(
                    &f.name,
                    &f.params,
                    &f.body.statements,
                    f.return_type.as_ref(),
                    false,
                ),
                Job::Equals(f) => FnCompiler::new(self, true).compile_callable(&f.name, &f.params, &f.body.statements, None, true),
                Job::Ctor(params, body) => {
                    FnCompiler::new(self, true).compile_callable(lang.syntax.constructor_function_name, params, body, None, false)
                }
                Job::Getter(body) => FnCompiler::new(self, true).compile_callable("", &[], body, None, false),
                Job::Setter(st) => {
                    let params: Vec<ast::Param> =
                        st.param.iter().map(|n| ast::Param { name: n.clone(), type_annotation: None, default: None }).collect();
                    FnCompiler::new(self, true).compile_callable("", &params, &st.body, None, true)
                }
                Job::Init(body) => FnCompiler::new(self, true).compile_field_init(body),
            };
            if let Job::Function(f) | Job::Method(f) = *job {
                proto.text = f.go_string().into();
            }
            self.prog.protos[base as usize + i + 1] = proto;
        }

        let classes = self.class_ids.iter().map(|(n, id)| (symbol::intern(n), *id)).collect();
        self.prog.modules.push(ModuleInfo {
            key: key.to_string(),
            lang,
            main: base,
            globals: self.globals.iter().map(|(n, s)| (n.clone(), s.loc_global())).collect(),
            global_range: (global_start, self.prog.globals.len() as u32),
            functions: self.functions.clone(),
            all_functions,
            classes,
            interfaces,
            error_class: symbol::intern(lang.error_class),
        });
    }

    /// The names an import binds where it stands (its items, or with `전부`
    /// the functions of the module).
    fn import_names(&self, module: &str, is_builtin: bool, all: bool, items: &[ast::ImportItem]) -> Vec<String> {
        let mut names: Vec<String> =
            items.iter().map(|i| if i.alias.is_empty() { i.name.clone() } else { i.alias.clone() }).collect();
        if all {
            if is_builtin {
                if let Some((true, fns)) = (self.std_lookup)(self.lang.name, module) {
                    names.extend(fns);
                }
            } else if let Some(Source::Ok(p, _)) = self.sources.get(&format!("file:{module}")) {
                for s in &p.statements {
                    if let Stmt::Function(f) = s {
                        names.push(f.name.clone());
                    }
                }
            }
        }
        names
    }

    /// The names a scope's statements can declare, with whether any declaration
    /// of the name has a type or is a constant (it then needs a meta slot).
    /// Blocks of `만약`/`따라 나누자`/`일단 해보자` belong to the scope; loops,
    /// functions and catch handlers open their own. An import declares what it binds.
    fn declared_names(&self, stmts: &[Stmt]) -> Vec<(String, bool)> {
        fn add(out: &mut Vec<(String, bool)>, name: &str, meta: bool) {
            match out.iter_mut().find(|(n, _)| n == name) {
                Some(e) => e.1 |= meta,
                None => out.push((name.to_string(), meta)),
            }
        }
        fn go(c: &Compiler, stmts: &[Stmt], out: &mut Vec<(String, bool)>) {
            for s in stmts {
                match s {
                    Stmt::VarDecl(v) if !v.is_static => {
                        if let Some(n) = &v.name {
                            add(out, n, v.type_ref.is_some() || v.is_constant);
                        }
                    }
                    Stmt::Input { target: Some(n), .. } => add(out, n, false),
                    Stmt::Import { module, is_builtin, all, items } => {
                        for n in c.import_names(module, *is_builtin, *all, items) {
                            add(out, &n, false);
                        }
                    }
                    Stmt::If { consequent, alternate, .. } => {
                        go(c, &consequent.statements, out);
                        if let Some(b) = alternate {
                            go(c, &b.statements, out);
                        }
                    }
                    Stmt::Switch { cases, .. } => {
                        for case in cases {
                            go(c, &case.consequent.statements, out);
                        }
                    }
                    Stmt::Try { block, finalizer, .. } => {
                        go(c, &block.statements, out);
                        if let Some(f) = finalizer {
                            go(c, &f.statements, out);
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        go(self, stmts, &mut out);
        out
    }
}

/// Constructs this version cannot run yet; they fail before anything runs.
fn check_supported(stmts: &[Stmt], lang: &Lang, std_lookup: StdLookup) -> Result<(), Unsupported> {
    let mut problem: Option<String> = None;
    walk_stmts(
        stmts,
        &mut |s| {
            if let Stmt::Import { module, is_builtin: true, .. } = s {
                if problem.is_none() {
                    let package = module.contains('/') || std::path::Path::new("packages").join(module).exists();
                    if package {
                        problem = Some(format!("package [{module}]"));
                    } else if let Some((false, _)) = std_lookup(lang.name, module) {
                        problem = Some(format!("standard module [{module}]"));
                    }
                }
            }
        },
        &mut |_| {},
    );
    match problem {
        Some(p) => Err(Unsupported(p)),
        None => Ok(()),
    }
}

/// What gets compiled into a function body.
enum Job<'a> {
    Function(&'a ast::FuncDecl),
    Method(&'a ast::FuncDecl),
    /// A class's `<기호 같다>` for `==`: binds only the first argument.
    Equals(&'a ast::FuncDecl),
    Ctor(&'a [ast::Param], &'a [Stmt]),
    Getter(&'a [Stmt]),
    /// Binds only the new value (to the parameter, when it has one).
    Setter(&'a ast::Setter),
    /// A class's field initializers, for objects made where the class is not
    /// known when compiling (an imported class).
    Init(&'a [Stmt]),
}

/// The compiled parts of one class, in declaration order.
#[derive(Default)]
struct ClassProtos {
    /// (name, proto, static, access)
    methods: Vec<(String, u32, bool, Access)>,
    ctor: Option<u32>,
    equals: Option<u32>,
    init: u32,
    fields: Vec<FieldDecl>,
}

/// A field (or property) as its class declares it.
struct FieldDecl {
    name: String,
    access: Access,
    getter: Option<u32>,
    setter: Option<u32>,
    /// Declared type id (0: none); None for a static field.
    ty: Option<u32>,
}

/// Hana's built-in `[오류]`: a `메시지` field and a constructor that sets it.
fn builtin_error_class(lang: &Lang) -> Stmt {
    let this_msg = Expr::Member {
        object: Box::new(Expr::Identifier(lang.self_words[0].to_string())),
        property: Box::new(Expr::Identifier(lang.error_message.to_string())),
    };
    Stmt::Class {
        name: Some(ast::TypeRef { name: lang.error_class.to_string() }),
        base: None,
        interfaces: Vec::new(),
        is_abstract: false,
        body: vec![
            Stmt::VarDecl(ast::VarDecl {
                name: Some(lang.error_message.to_string()),
                type_ref: None,
                value: Some(Expr::Str(String::new())),
                is_constant: false,
                access: "public",
                is_static: false,
                getter: None,
                setter: None,
            }),
            Stmt::Constructor {
                id: lang.syntax.constructor_function_name.to_string(),
                params: vec![ast::Param { name: lang.error_ctor_arg.to_string(), type_annotation: None, default: None }],
                body: vec![Stmt::Assign { target: this_msg.clone(), value: Some(Expr::Identifier(lang.error_ctor_arg.to_string())) }],
            },
            Stmt::Function(ast::FuncDecl {
                name: "__toString__".to_string(),
                params: Vec::new(),
                body: Block { statements: vec![Stmt::Return(Some(this_msg))] },
                access: "public",
                is_static: false,
                return_type: None,
            }),
        ],
    }
}

fn placeholder(name: &str, module: u32) -> Proto {
    Proto {
        name: name.to_string(),
        module,
        raw_params: false,
        code: Vec::new(),
        nregs: 0,
        params: Vec::new(),
        return_type: 0,
        loops: Vec::new(),
        handlers: Vec::new(),
        text: "".into(),
    }
}

impl Slot {
    fn loc_global(&self) -> u32 {
        match self.loc {
            Loc::Global(g) => g,
            _ => unreachable!(),
        }
    }
}

struct Compiler<'a> {
    prog: Program,
    type_ids: HashMap<(String, &'static str), u32>,
    str_consts: HashMap<String, u32>,
    sources: &'a HashMap<String, Source>,
    std_lookup: StdLookup<'a>,
    module_ids: HashMap<String, u32>,

    // The module being compiled.
    lang: &'static Lang,
    module: u32,
    globals: HashMap<String, Slot>,
    functions: HashMap<String, u32>,
    /// Class declarations by name (the built-in error class included).
    class_defs: HashMap<String, &'a Stmt>,
    /// Class ids by name.
    class_ids: HashMap<String, u32>,
}

impl<'a> Compiler<'a> {
    fn name(&mut self, s: &str) -> u32 {
        symbol::intern(s)
    }

    /// The class and its ancestors that exist, then the first missing
    /// parent's name if any (Hana still compares that name).
    fn chain(&self, name: &str) -> (Vec<String>, Option<String>) {
        let mut out: Vec<String> = Vec::new();
        let mut cur = name.to_string();
        loop {
            if out.contains(&cur) {
                return (out, None); // a cycle: Hana would never finish
            }
            let Some(Stmt::Class { base, .. }) = self.class_defs.get(&cur).copied() else {
                return (out, Some(cur));
            };
            out.push(cur);
            match base {
                Some(b) => cur = b.name.clone(),
                None => return (out, None),
            }
        }
    }

    /// Resolves every class's lookups ahead (Hana's `classMember`,
    /// `fieldAnnotation`, constructor search and subtype checks).
    fn build_classes(&mut self, order: &[String], protos: &HashMap<String, ClassProtos>) {
        for name in order {
            let (chain, missing) = self.chain(name);
            let Some(Stmt::Class { base, interfaces, is_abstract, .. }) = self.class_defs.get(name).copied() else {
                continue;
            };
            let mut info = ClassInfo {
                name: symbol::intern(name),
                is_abstract: *is_abstract,
                members: HashMap::new(),
                field_types: HashMap::new(),
                ctor: None,
                statics: HashMap::new(),
                equals: protos[name].equals,
                init: protos[name].init,
                lineage: Vec::new(),
                supertypes: HashSet::new(),
                super_start: base.as_ref().map(|b| self.class_defs.contains_key(&b.name).then(|| symbol::intern(&b.name))),
            };
            let mut field_seen: HashSet<u32> = HashSet::new();
            let mut method_seen: HashSet<u32> = HashSet::new();
            for class in &chain {
                let cp = &protos[class];
                for FieldDecl { name: fname, access, getter, setter, ty } in &cp.fields {
                    let sym = symbol::intern(fname);
                    if field_seen.insert(sym) {
                        let m = info.members.entry(sym).or_default();
                        m.field_access = *access;
                        m.getter = *getter;
                        m.setter = *setter;
                    }
                    if let Some(ty) = ty {
                        info.field_types.entry(sym).or_insert(*ty);
                    }
                }
                for (mname, proto, _, access) in &cp.methods {
                    let sym = symbol::intern(mname);
                    if method_seen.insert(sym) {
                        let m = info.members.entry(sym).or_default();
                        m.method = Some(*proto);
                        m.method_access = *access;
                    }
                }
                if info.ctor.is_none() {
                    info.ctor = cp.ctor;
                }
                let sym = symbol::intern(class);
                info.lineage.push(sym);
                info.supertypes.insert(sym);
                if let Some(Stmt::Class { interfaces, .. }) = self.class_defs.get(class).copied() {
                    for i in interfaces {
                        info.supertypes.insert(symbol::intern(&i.name));
                    }
                }
            }
            if let Some(m) = missing {
                let sym = symbol::intern(&m);
                info.lineage.push(sym);
                info.supertypes.insert(sym);
            }
            let _ = interfaces;
            for (mname, proto, is_static, _) in &protos[name].methods {
                if *is_static {
                    info.statics.entry(symbol::intern(mname)).or_insert(*proto);
                }
            }
            self.class_ids.insert(name.clone(), self.prog.classes.len() as u32);
            self.prog.classes.push(info);
        }
    }

    /// Hana checks, before running, that each class has the methods its
    /// interfaces require (its own body only).
    fn interface_violation(&self, stmts: &[Stmt]) -> Option<(String, String, String)> {
        let ifaces: HashMap<&str, &Vec<Stmt>> = stmts
            .iter()
            .filter_map(|s| match s {
                Stmt::Interface { name: Some(n), body } => Some((n.name.as_str(), body)),
                _ => None,
            })
            .collect();
        for s in stmts {
            let Stmt::Class { name: Some(n), interfaces, body, .. } = s else { continue };
            // A later declaration of the name replaces this one.
            if !std::ptr::eq(self.class_defs.get(&n.name).copied().unwrap_or(s), s) {
                continue;
            }
            for i in interfaces {
                let Some(required) = ifaces.get(i.name.as_str()) else { continue };
                for r in required.iter() {
                    let Stmt::InterfaceMethod(m) = r else { continue };
                    let has = body.iter().any(|b| matches!(b, Stmt::Function(f) if &f.name == m));
                    if !has {
                        return Some((n.name.clone(), i.name.clone(), m.clone()));
                    }
                }
            }
        }
        None
    }

    fn type_id(&mut self, text: &str) -> u32 {
        let key = (text.to_string(), self.lang.name);
        if let Some(&i) = self.type_ids.get(&key) {
            return i;
        }
        let i = self.prog.types.len() as u32;
        self.prog.types.push(TypeSpec::parse(text, self.lang));
        self.type_ids.insert(key, i);
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

struct Scope {
    names: HashMap<String, Slot>,
}

struct LoopCtx {
    breaks: Vec<usize>,
    /// `마무리는 항상` blocks open when the loop started: a break that has to
    /// pass more of them goes as a signal, so they run.
    finally_depth: u32,
}

struct FnCompiler<'c, 'a> {
    c: &'c mut Compiler<'a>,
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
    /// A method, constructor, getter or setter: the object's properties read
    /// like variables (between the call's scope and the globals).
    has_this: bool,
    is_main: bool,
    /// `마무리는 항상` blocks around the code being compiled.
    finally_depth: u32,
}

impl<'c, 'a> FnCompiler<'c, 'a> {
    fn new(c: &'c mut Compiler<'a>, has_this: bool) -> FnCompiler<'c, 'a> {
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
            has_this,
            is_main: false,
            finally_depth: 0,
        }
    }

    fn finish(self, name: &str, params: Vec<Param>, return_type: u32, raw_params: bool) -> Proto {
        Proto {
            name: name.to_string(),
            module: self.c.module,
            raw_params,
            code: self.code,
            nregs: self.max_reg,
            params,
            return_type,
            loops: self.loop_ranges,
            handlers: self.handlers,
            text: "".into(),
        }
    }

    fn compile_main(mut self, stmts: &[Stmt]) -> Proto {
        self.is_main = true;
        // Hana validates interfaces before running anything.
        if let Some((class, iface, method)) = self.c.interface_violation(stmts) {
            let k = self.c.str_const("InterfaceImplementationError.InterfaceNotImplemented");
            let args = [class, iface, method].map(|a| self.c.str_const(&a));
            self.emit(Op::Fail { k, args });
        }
        for s in stmts {
            match s {
                // A class's statement sets its static variables, in order.
                Stmt::Class { name: Some(n), body, .. } => self.class_statics(&n.name, body),
                _ => self.stmt(s),
            }
        }
        self.emit(Op::ReturnNull);
        self.finish("<main>", Vec::new(), 0, false)
    }

    /// `'우리'의 '수'를 0으로 정하자` in a class body, run when the program
    /// reaches the class (in the top level's scope).
    fn class_statics(&mut self, class: &str, body: &[Stmt]) {
        let class = self.c.name(class);
        for s in body {
            let mark = self.next_reg;
            match s {
                Stmt::VarDecl(v) if v.is_static => {
                    let r = self.alloc();
                    match &v.value {
                        Some(e) => self.expr_to(e, r),
                        None => {
                            self.emit(Op::LoadNull { dst: r });
                        }
                    }
                    let name = self.c.name(v.name.as_deref().unwrap_or(""));
                    self.emit(Op::SetStatic { class, name, src: r });
                }
                Stmt::Assign { target: Expr::Member { object, property }, value } => {
                    let plural = matches!(&**object, Expr::Identifier(o) if self.c.lang.plural_self_words.contains(&o.as_str()));
                    if let (true, Expr::Identifier(p)) = (plural, &**property) {
                        let r = self.alloc();
                        match value {
                            Some(e) => self.expr_to(e, r),
                            None => {
                                self.emit(Op::LoadNull { dst: r });
                            }
                        }
                        let name = self.c.name(p);
                        self.emit(Op::SetStatic { class, name, src: r });
                    }
                }
                _ => {}
            }
            self.next_reg = mark;
        }
    }

    /// A function, method, constructor, getter or setter body. `raw` binds
    /// only the first argument, without checks (setters, `<기호 같다>`).
    fn compile_callable(
        mut self,
        name: &str,
        params_ast: &[ast::Param],
        body: &[Stmt],
        return_type: Option<&ast::TypeRef>,
        raw: bool,
    ) -> Proto {
        // Parameters take the first registers: arguments arrive there.
        let mut scope = Scope { names: HashMap::new() };
        let mut params = Vec::new();
        let first_regs: Vec<Reg> = params_ast.iter().map(|_| self.alloc()).collect();
        for (i, p) in params_ast.iter().enumerate() {
            let ty = if raw { 0 } else { p.type_annotation.as_ref().map_or(0, |t| self.c.type_id(&t.name)) };
            let meta = (ty != 0).then(|| self.alloc());
            let slot = Slot { loc: Loc::Reg(first_regs[i]), meta: meta.map(Loc::Reg) };
            scope.names.insert(p.name.clone(), slot);
            params.push(Param { name: self.c.name(&p.name), slot: first_regs[i], meta, ty });
        }
        // A repeated parameter name is one variable: the later parameter's.
        for (n, flags) in self.c.declared_names(body) {
            match scope.names.get_mut(&n) {
                Some(slot) => {
                    if flags && slot.meta.is_none() {
                        slot.meta = Some(Loc::Reg(self.alloc()));
                    }
                }
                None => {
                    let loc = Loc::Reg(self.alloc());
                    let meta = flags.then(|| Loc::Reg(self.alloc()));
                    scope.names.insert(n, Slot { loc, meta });
                }
            }
        }
        // Parameter metas must point at the scope's final slots.
        for (p, ast_p) in params.iter_mut().zip(params_ast) {
            let slot = scope.names[&ast_p.name];
            p.meta = slot.meta.map(|m| match m {
                Loc::Reg(r) => r,
                _ => unreachable!(),
            });
        }
        self.scopes.push(scope);

        if !raw {
            // Missing arguments: defaults (evaluated in the call's scope) or an error.
            for (i, p) in params_ast.iter().enumerate() {
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
            // A parameter's slot is defined from here on, unless another
            // name's declaration shares it (a repeated parameter name).
            for (p, ast_p) in params.iter().zip(params_ast) {
                if let Some(Slot { loc: Loc::Reg(r), .. }) = self.scopes[0].names.get(&ast_p.name).copied() {
                    if r == p.slot {
                        self.definite.push(r);
                    }
                }
            }
        }

        self.stmts(body);
        self.emit(Op::ReturnNull);
        let return_type = return_type.map_or(0, |t| self.c.type_id(&t.name));
        self.finish(name, params, return_type, raw)
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
        if self.has_this {
            slots.push(Slot { loc: Loc::This(self.c.name(name)), meta: None });
        }
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
            [Slot { loc: Loc::Reg(slot), .. }] => Op::GetReg { dst, slot: *slot, name: v.name },
            [Slot { loc: Loc::Global(slot), .. }] => Op::GetGlobal { dst, slot: *slot, name: v.name },
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
        for (name, flags) in self.c.declared_names(stmts) {
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
            _ => unreachable!(),
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
            // `'우리'의 '수'를 ...로 정하자`: a static of the running method's class.
            Stmt::VarDecl(v) if v.is_static => {
                let r = self.alloc();
                match &v.value {
                    Some(e) => self.expr_to(e, r),
                    None => {
                        self.emit(Op::LoadNull { dst: r });
                    }
                }
                let name = self.c.name(v.name.as_deref().unwrap_or(""));
                self.emit(Op::SetStatic { class: NONE, name, src: r });
            }
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
                // At the top level, or where `마무리는 항상` must run first, the
                // return travels as a signal.
                if self.is_main || self.finally_depth > 0 {
                    self.emit(Op::ReturnSignal { src: r });
                } else {
                    self.emit(Op::Return { src: r });
                }
            }
            Stmt::Break => match self.loops.last() {
                Some(l) if l.finally_depth == self.finally_depth => {
                    let at = self.emit(Op::Jump { to: 0 });
                    self.loops.last_mut().unwrap().breaks.push(at);
                }
                _ => {
                    self.emit(Op::Break);
                }
            },
            Stmt::Throw(e) => {
                let r = self.alloc();
                self.expr_to(e, r);
                self.emit(Op::Throw { src: r });
            }
            Stmt::Try { block, handlers, finalizer } => self.try_stmt(block, handlers, finalizer.as_ref()),
            Stmt::Import { module, is_builtin, all, items } => self.import(module, *is_builtin, *all, items),
            // Declarations Hana does not run where they stand.
            Stmt::Class { .. } | Stmt::Interface { .. } | Stmt::Constructor { .. } | Stmt::InterfaceMethod(_) => {}
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
                let front = *position == "front";
                self.emit(Op::ListPush { list, val, front, target: target_var });
                // A list in an object's field: Hana evaluates the object again
                // and checks the field's declared type.
                if let Expr::Member { object, property } = target {
                    if let Expr::Identifier(p) = &**property {
                        let obj = self.alloc();
                        self.expr_to(object, obj);
                        let name = self.c.name(p);
                        self.emit(Op::CheckFieldPush { list, obj, name, front });
                    }
                }
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
                    Loc::Reg(slot) => Some(Op::SetReg { slot, src }),
                    Loc::Global(slot) => Some(Op::SetGlobal { slot, src }),
                    Loc::This(_) => None,
                };
                if let Some(op) = op {
                    self.emit(op);
                    return;
                }
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
                let name = match &**property {
                    Expr::Identifier(n) => self.c.name(n),
                    _ => NONE,
                };
                let set = self.emit(Op::SetMember { obj, val, name, skip: 0 });
                let key = self.alloc();
                let start = self.here();
                self.expr_to(property, key);
                let end = self.here();
                self.emit(Op::SetIndex { obj, key, val });
                let done = self.emit(Op::Jump { to: 0 });
                let target = self.here();
                let key = self.handlers.len() as u32;
                let fail = self.emit(Op::SetIndexFail { obj, val, name, key, skip: 0 });
                self.handlers.push(Handler { start, end, target, open_calls: self.open_calls, kind: HandlerKind::Protect });
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

    /// Where a name an import binds goes: the innermost scope (Hana's
    /// `env.Declare`), which declared it.
    fn bind_slot(&self, name: &str) -> Loc {
        match self.scopes.last() {
            Some(scope) => scope.names[name].loc,
            None => self.c.globals[name].loc,
        }
    }

    /// `...에서 ...을 가져오자`: loads the module when it runs, then binds.
    fn import(&mut self, module: &str, is_builtin: bool, all: bool, items: &[ast::ImportItem]) {
        let native = items.iter().any(|i| i.name.starts_with(self.c.lang.native_prefix));
        let kind = if is_builtin && native {
            // A package's native library (Hana's ABI); there is no such package here.
            ImportKind::Fail { code: "ImportError.ImportDLLNotFound".to_string(), args: vec![module.to_string()] }
        } else if is_builtin {
            ImportKind::Std(module.to_string())
        } else {
            let key = format!("file:{module}");
            match (self.c.module_ids.get(&key), self.c.sources.get(&key)) {
                (Some(&m), _) => ImportKind::File(m),
                (None, Some(Source::Fail(code, arg))) => ImportKind::Fail { code: code.to_string(), args: vec![arg.clone()] },
                _ => ImportKind::Fail { code: "ImportError.ImportFileNotFound".to_string(), args: vec![module.to_string()] },
            }
        };
        let items: Vec<(String, String, Loc)> = items
            .iter()
            .map(|i| {
                let bind = if i.alias.is_empty() { i.name.clone() } else { i.alias.clone() };
                let slot = self.bind_slot(&bind);
                (i.name.clone(), bind, slot)
            })
            .collect();
        let aliased = items.iter().filter(|(n, b, _)| n != b).map(|(n, _, _)| symbol::intern(n)).collect();
        let mut all_slots = HashMap::new();
        if all {
            for n in self.c.import_names(module, is_builtin, true, &[]) {
                let slot = self.bind_slot(&n);
                all_slots.insert(n, slot);
            }
        }
        let import = self.c.prog.imports.len() as u32;
        self.c.prog.imports.push(ImportInfo { kind, source: module.to_string(), all, items, all_slots, aliased });
        self.emit(Op::Import { import });
    }

    /// A class's field initializers as a body of their own (run on `this`).
    fn compile_field_init(mut self, body: &[Stmt]) -> Proto {
        let obj = self.alloc();
        self.emit(Op::GetThis { dst: obj });
        for s in body {
            let mark = self.next_reg;
            match s {
                Stmt::VarDecl(v) if !v.is_static => {
                    let r = self.alloc();
                    match &v.value {
                        Some(e) => self.expr_to(e, r),
                        None => {
                            self.emit(Op::LoadNull { dst: r });
                        }
                    }
                    let name = self.c.name(v.name.as_deref().unwrap_or(""));
                    let ty = v.type_ref.as_ref().map_or(0, |t| self.c.type_id(&t.name));
                    self.emit(Op::InitField { obj, name, src: r, ty });
                }
                Stmt::Assign { target: Expr::Identifier(n), value } => {
                    let r = self.alloc();
                    match value {
                        Some(e) => self.expr_to(e, r),
                        None => {
                            self.emit(Op::LoadNull { dst: r });
                        }
                    }
                    let name = self.c.name(n);
                    self.emit(Op::InitField { obj, name, src: r, ty: 0 });
                }
                _ => {}
            }
            self.next_reg = mark;
        }
        self.emit(Op::ReturnNull);
        self.finish("", Vec::new(), 0, false)
    }

    /// `일단 해보자`: the block, handlers chosen by the thrown value's class,
    /// and a `마무리는 항상` that runs on every way out.
    fn try_stmt(&mut self, block: &Block, handlers: &[ast::CatchClause], finalizer: Option<&Block>) {
        if finalizer.is_some() {
            self.finally_depth += 1;
        }
        let start = self.here();
        self.stmts(&block.statements);
        let block_end = self.here();
        let mut to_finally = vec![self.emit(Op::Jump { to: 0 })];
        if !handlers.is_empty() {
            let key = self.handlers.len() as u32;
            let dispatch = self.here();
            self.handlers.push(Handler { start, end: block_end, target: dispatch, open_calls: self.open_calls, kind: HandlerKind::Catch });
            let mut entries = Vec::new();
            for h in handlers {
                entries.push(match &h.type_ref {
                    None => self.emit(Op::Jump { to: 0 }),
                    Some(t) => {
                        let class = self.c.name(&t.name);
                        self.emit(Op::CatchIs { key, class, to: 0 })
                    }
                });
            }
            // No handler fits: raise it again (inside the finally's range).
            self.emit(Op::Rethrow { key });
            for (h, entry) in handlers.iter().zip(entries) {
                let here = self.here();
                match &mut self.code[entry] {
                    Op::CatchIs { to, .. } | Op::Jump { to } => *to = here,
                    _ => unreachable!(),
                }
                let (from, _) = self.open_scope(&[h.param.as_str()], &h.body.statements);
                let slot = self.slot_reg(&h.param);
                self.emit(Op::CatchBind { key, slot });
                self.stmts(&h.body.statements);
                self.close_scope(from);
                to_finally.push(self.emit(Op::Jump { to: 0 }));
            }
        }
        let handlers_end = self.here();
        if finalizer.is_some() {
            self.finally_depth -= 1;
        }
        let normal = self.here();
        for j in to_finally {
            self.patch_jump(j, normal);
        }
        if let Some(f) = finalizer {
            self.stmts(&f.statements);
            let done = self.emit(Op::Jump { to: 0 });
            let key = self.handlers.len() as u32;
            let target = self.here();
            self.handlers.push(Handler { start, end: handlers_end, target, open_calls: self.open_calls, kind: HandlerKind::Finally });
            self.stmts(&f.statements);
            self.emit(Op::Rethrow { key });
            let end = self.here();
            self.patch_jump(done, end);
        }
    }

    fn loop_body(&mut self, body: &Block, top: u32) -> Vec<usize> {
        self.loops.push(LoopCtx { breaks: Vec::new(), finally_depth: self.finally_depth });
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
        let name = if loop_var.is_empty() { self.c.lang.default_index } else { loop_var };
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
        let lang = self.c.lang;
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
            Expr::Identifier(n) if self.c.lang.self_words.contains(&n.as_str()) => {
                let var = self.var(n);
                self.emit(Op::SelfOr { dst, var });
            }
            Expr::Identifier(n) if self.c.lang.plural_self_words.contains(&n.as_str()) => {
                let var = self.var(n);
                self.emit(Op::StaticOr { dst, var });
            }
            Expr::Identifier(n) => self.get_var(n, dst),
            Expr::SelfRef => {
                self.emit(Op::GetThis { dst });
            }
            Expr::StaticRef => {
                self.emit(Op::GetStatic { dst });
            }
            // A type used as a value: a variable of that name, the class, or the name.
            Expr::TypeRef(t) => {
                let var = self.var(&t.name);
                let name = self.c.name(&t.name);
                self.emit(Op::TypeValue { dst, var, name });
            }
            Expr::New { class, args } => self.new_object(class.as_ref(), args, dst),
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
        if op == "instanceof" {
            let b = self.operand(right);
            self.emit(Op::InstanceOf { dst, a, b });
            return;
        }
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
        if matches!(object, Expr::SuperRef) {
            // `부모의` names only methods; a method as a value comes later.
            self.emit(Op::SuperPrep { method: matches!(property, Expr::FunctionRef(_)) });
            self.unsupported("method value");
            return;
        }
        let obj = self.alloc();
        self.expr_to(object, obj);
        let name = match property {
            Expr::Identifier(n) => self.c.name(n),
            // A class's static read by a number names it as Go's `%g` would.
            Expr::Number(n) => self.c.name(&crate::format::go_v_float(*n)),
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
        let key = self.handlers.len() as u32;
        self.emit(Op::IndexFail { obj, key });
        self.handlers.push(Handler { start, end, target, open_calls: self.open_calls, kind: HandlerKind::Protect });
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
        let quote = self.c.lang.var_quote;
        let quoted = |n: &str| n.starts_with(quote.0) && n.ends_with(quote.1) && n.len() > quote.0.len() + quote.1.len();
        match callee {
            // `<'변수'>()`: the function the variable names.
            Expr::FunctionRef(name) if quoted(name) => {
                let callee = self.alloc();
                self.reflect(name, callee, true);
                let base = self.args(args);
                self.emit(Op::CallValue { dst, callee, base, argc });
            }
            // `'객체'의 <'변수'>()`: the method the variable names.
            Expr::Member { object, property } if matches!(&**property, Expr::FunctionRef(n) if quoted(n)) => {
                let Expr::FunctionRef(name) = &**property else { unreachable!() };
                let method = self.alloc();
                self.reflect(name, method, false);
                let obj = self.alloc();
                self.expr_to(object, obj);
                self.emit(Op::MethodPrepDyn { obj, name: method });
                let base = self.args(args);
                self.emit(Op::CallMethodDyn { dst, obj, name: method, base, argc });
            }
            Expr::FunctionRef(name) => self.call_named(name, args, dst),
            // `TYPE의 〈함수〉()` where TYPE is not a class: a plain call.
            Expr::Member { object, property }
                if matches!(&**object, Expr::TypeRef(t) if !self.c.class_defs.contains_key(&t.name))
                    && matches!(&**property, Expr::FunctionRef(_)) =>
            {
                let Expr::FunctionRef(name) = &**property else { unreachable!() };
                self.call_named(name, args, dst);
            }
            // `부모의 <메서드>()`: the method as the parent class has it.
            Expr::Member { object, property } if matches!(&**object, Expr::SuperRef) => {
                let method = matches!(&**property, Expr::FunctionRef(_));
                self.emit(Op::SuperPrep { method });
                if let Expr::FunctionRef(name) = &**property {
                    let name = self.c.name(name);
                    let base = self.args(args);
                    self.emit(Op::CallSuper { dst, name, base, argc });
                }
            }
            Expr::Member { object, property } if matches!(&**property, Expr::FunctionRef(_)) => {
                let Expr::FunctionRef(name) = &**property else { unreachable!() };
                let obj = self.alloc();
                self.expr_to(object, obj);
                let name = self.c.name(name);
                self.emit(Op::MethodPrep { obj, name });
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

    /// `새로운 [클래스](...)`: the object, its class's own field initializers
    /// (evaluated here, in the caller's scope, as Hana does), then the
    /// constructor with the arguments; without a constructor the arguments
    /// are not evaluated.
    fn new_object(&mut self, class: Option<&ast::TypeRef>, args: &[Expr], dst: Reg) {
        self.emit(Op::Enter);
        self.open_calls += 1;
        let name = class.map_or("", |t| t.name.as_str()).to_string();
        let sym = self.c.name(&name);
        self.emit(Op::NewObj { dst, class: sym });
        if let Some(Stmt::Class { body, .. }) = self.c.class_defs.get(&name).copied() {
            for s in body {
                let mark = self.next_reg;
                match s {
                    Stmt::VarDecl(v) if !v.is_static => {
                        let r = self.alloc();
                        match &v.value {
                            Some(e) => self.expr_to(e, r),
                            None => {
                                self.emit(Op::LoadNull { dst: r });
                            }
                        }
                        let fname = self.c.name(v.name.as_deref().unwrap_or(""));
                        let ty = v.type_ref.as_ref().map_or(0, |t| self.c.type_id(&t.name));
                        self.emit(Op::InitField { obj: dst, name: fname, src: r, ty });
                    }
                    Stmt::Assign { target: Expr::Identifier(n), value } => {
                        let r = self.alloc();
                        match value {
                            Some(e) => self.expr_to(e, r),
                            None => {
                                self.emit(Op::LoadNull { dst: r });
                            }
                        }
                        let fname = self.c.name(n);
                        self.emit(Op::InitField { obj: dst, name: fname, src: r, ty: 0 });
                    }
                    _ => {}
                }
                self.next_reg = mark;
            }
        }
        if !self.c.class_defs.contains_key(&name) {
            // A class this module does not declare (imported): found when it runs.
            self.emit(Op::InitFieldsDyn { obj: dst });
            let base = self.args(args);
            self.emit(Op::CallCtorDyn { obj: dst, base, argc: args.len() as u16 });
            self.open_calls -= 1;
            return;
        }
        let ctor = self.c.class_ids.get(&name).and_then(|&id| self.c.prog.classes[id as usize].ctor);
        match ctor {
            Some(proto) => {
                let base = self.args(args);
                self.emit(Op::CallCtor { obj: dst, proto, class: sym, base, argc: args.len() as u16 });
            }
            None => {
                self.emit(Op::Leave);
            }
        }
        self.open_calls -= 1;
    }

    /// A reflected name `'변수'` (quotes included): `resolve` gives the
    /// function value it names, else just the name.
    fn reflect(&mut self, quoted: &str, dst: Reg, resolve: bool) {
        let q = self.c.lang.var_quote;
        let inner = &quoted[q.0.len()..quoted.len() - q.1.len()];
        let var = self.var(inner);
        let site = self.c.prog.consts.len() as u32; // a unique number per site
        self.c.prog.consts.push(Value::NULL);
        let quoted = self.c.name(quoted);
        self.emit(Op::Reflect { dst, site: if resolve { site } else { site | 1 << 31 }, var, quoted });
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
                    let e = parse_embedded(&code, self.c.lang);
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
