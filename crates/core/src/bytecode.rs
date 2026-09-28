//! The compiled form of a program: register-machine code per function.
//!
//! Every variable is resolved at compile time to the slots it can live in
//! (see [`Var`]); nothing is looked up by name while running.

use std::collections::{HashMap, HashSet};

use crate::lang::Lang;
use crate::value::Value;

pub type Reg = u16;

/// Where a variable lives: a register of the running function's frame, or a
/// global of the program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Loc {
    Reg(Reg),
    Global(u32),
    /// A property of the running method's object (Hana's `Environment.this`):
    /// inside a method, the object's properties read like variables.
    This(u32),
}

/// A variable slot and, when some declaration of it carries a type or is a
/// constant, the slot that remembers that ([`Meta`]).
#[derive(Clone, Copy, Debug)]
pub struct Slot {
    pub loc: Loc,
    pub meta: Option<Loc>,
}

/// A variable as a reference sees it: the scopes that may hold a variable of
/// that name, innermost first. The first one whose slot is defined is the
/// variable (the environment chain of Hana's tree-walker, decided ahead).
#[derive(Clone, Debug)]
pub struct Var {
    pub name: u32,
    pub slots: Vec<Slot>,
}

/// What a meta slot holds (a number): the declared type id times two, plus
/// one for a constant. Undefined or null means neither.
pub fn meta_value(type_id: u32, constant: bool) -> Value {
    Value::num((type_id * 2 + constant as u32) as f64)
}

pub fn meta_parts(v: &Value) -> (u32, bool) {
    match v.as_num() {
        Some(n) => {
            let n = n as u32;
            (n / 2, n % 2 == 1)
        }
        None => (0, false),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Gt,
    Lt,
    Ge,
    Le,
}

impl BinOp {
    pub fn is_comparison(self) -> bool {
        matches!(self, BinOp::Gt | BinOp::Lt | BinOp::Ge | BinOp::Le)
    }

    pub fn symbol(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Mod => "%",
            BinOp::Gt => ">",
            BinOp::Lt => "<",
            BinOp::Ge => ">=",
            BinOp::Le => "<=",
        }
    }
}

/// No variable / no name.
pub const NONE: u32 = u32::MAX;

#[derive(Clone, Copy, Debug)]
pub enum Op {
    LoadK { dst: Reg, k: u32 },
    LoadNull { dst: Reg },
    LoadBool { dst: Reg, v: bool },
    Move { dst: Reg, src: Reg },

    /// Reads a variable (first defined slot of the chain).
    GetVar { dst: Reg, var: u32 },
    /// Reads a variable that can only be in one register.
    GetReg { dst: Reg, slot: Reg, name: u32 },
    GetGlobal { dst: Reg, slot: u32, name: u32 },
    /// `정하자`: assign the variable if it exists, else declare it in the
    /// innermost scope (the chain's first slot). `ty` is the statement's
    /// annotation (0: none).
    Decl { var: u32, src: Reg, ty: u32, konst: bool },
    /// Stores into a register slot that has no type or constant to honour.
    SetReg { slot: Reg, src: Reg },
    SetGlobal { slot: u32, src: Reg },
    /// An assignment statement (`더하자`): only an existing variable changes.
    Assign { var: u32, src: Reg },
    /// `'x'에 값을 더하자` / `빼자`: `a` holds the variable's value as read,
    /// `b` the operand. Appends to a string in place when nothing else holds it.
    Update { var: u32, a: Reg, b: Reg, op: BinOp },
    /// `'x'에 1을 더하자` with a number constant, in its common case: a
    /// variable of one slot (not a constant, its type taking numbers) that
    /// holds a number changes here and the code goes on at `skip`. Otherwise
    /// the instructions after it (reading the variable, the constant,
    /// `Update`) do it.
    UpdateK { var: u32, k: u32, op: BinOp, skip: u32 },
    /// Marks registers `from..to` as holding no variable (a new loop pass).
    Undef { from: Reg, to: Reg },

    Bin { op: BinOp, dst: Reg, a: Reg, b: Reg },
    /// `Bin` with a number constant on the right.
    BinK { op: BinOp, dst: Reg, a: Reg, k: u32 },
    /// `==` (or `!=` when `neg`).
    Eq { dst: Reg, a: Reg, b: Reg, neg: bool },
    /// `Eq` against a constant (a number or a string).
    EqK { dst: Reg, a: Reg, k: u32, neg: bool },
    /// A condition that is `==` / `!=` and its jump on being false, in one:
    /// on to `to`, or past the `JumpIfFalse { cond: dst }` that follows. That
    /// jump is for an object whose class decides `==` itself: its method's
    /// result lands in `dst` and the jump takes it from there.
    EqJump { a: Reg, b: Reg, neg: bool, dst: Reg, to: u32 },
    /// `EqJump` against a constant.
    EqKJump { a: Reg, k: u32, neg: bool, dst: Reg, to: u32 },
    /// Requires a boolean (a condition) and copies it.
    Truth { dst: Reg, src: Reg },
    /// Raises `UnknownOperator` for the operator named by constant `k`.
    UnknownOp { k: u32 },
    Jump { to: u32 },
    JumpIfFalse { cond: Reg, to: u32 },
    JumpIfTrue { cond: Reg, to: u32 },

    /// Checks a range's bounds and writes the step (1 or -1).
    RangePrep { start: Reg, end: Reg, step: Reg },
    /// Leaves the loop (to `exit`) once `v` has passed `end`.
    RangeTest { v: Reg, end: Reg, step: Reg, exit: u32 },
    RangeStep { v: Reg, step: Reg },
    /// The end of a range loop's pass: `RangeStep`, then `RangeTest` —
    /// back to `body` (the instruction after the `RangeTest`) while `v` has
    /// not passed `end`, else on.
    RangeNext { v: Reg, end: Reg, step: Reg, body: u32 },
    /// Normalizes a number as Hana's `num.Box` does (-0 becomes 0): the
    /// range loop's variable.
    Boxed { dst: Reg },
    /// The sequence a `마다` loop walks: a copy of a list, or a string's characters.
    IterPrep { dst: Reg, src: Reg },
    IterNext { iter: Reg, idx: Reg, dst: Reg, exit: u32 },

    Print { src: Reg, newline: bool },
    Input { var: u32, ty: u32 },

    /// Starts a call expression (Hana counts nesting to stop runaway recursion).
    Enter,
    /// Calls a function of the program; arguments in `base..base+argc`.
    Call { dst: Reg, proto: u32, base: Reg, argc: u16 },
    /// Calls what the name means at run time: a variable holding a
    /// function (a built-in), or nothing (an error).
    CallName { dst: Reg, name: u32, var: u32, base: Reg, argc: u16 },
    CallValue { dst: Reg, callee: Reg, base: Reg, argc: u16 },
    /// A method of a string or list (`<자르기>`, `<비우기>`); `target` is the
    /// variable holding the list (for the constant check) or NONE.
    CallMethod { dst: Reg, obj: Reg, name: u32, target: u32, base: Reg, argc: u16 },
    Return { src: Reg },
    ReturnNull,
    /// A break outside any loop of this function: leaves functions until a loop.
    Break,

    /// Parameter prologue: jumps to `skip` when argument `index` was passed.
    ArgGiven { index: u16, skip: u32 },
    /// Binds a parameter's default value (checking its type).
    BindParam { index: u16, src: Reg },
    MissingArg { index: u16 },

    MakeList { dst: Reg, base: Reg, n: u16 },
    MakeDict { dst: Reg, base: Reg, n: u16 },
    /// A member read, first half: answers `길이` itself (jumping to `skip`),
    /// fails for values without members, or lets the key be computed.
    Member { dst: Reg, obj: Reg, name: u32, skip: u32 },
    Index { dst: Reg, obj: Reg, key: Reg },
    /// A comparison of two registers and the jump on its being false, in one
    /// (a condition of 만약 / 동안 반복).
    CmpJump { op: BinOp, a: Reg, b: Reg, to: u32 },
    /// `CmpJump` against a constant.
    CmpKJump { op: BinOp, a: Reg, k: u32, to: u32 },
    /// `'표'의 "가"` for a dictionary that has the key: its value, and on at
    /// `skip`. Anything else goes on to the instructions after it (the
    /// member read, the key, their errors).
    DictK { dst: Reg, obj: Reg, k: u32, skip: u32 },
    /// `'표'의 "가"를 ...로 정하자` for a dictionary, then on at `skip`;
    /// anything else goes on to the instructions after it.
    DictSetK { obj: Reg, k: u32, val: Reg, skip: u32 },
    /// `Index` with a constant key (`'표'의 "가"`).
    IndexK { dst: Reg, obj: Reg, k: u32 },
    /// Where a failed key computation lands (a list or string reports its own error).
    IndexFail { obj: Reg, key: u32 },
    /// A member write, first half: strings refuse, other values ignore it.
    SetMember { obj: Reg, val: Reg, name: u32, skip: u32 },
    SetIndex { obj: Reg, key: Reg, val: Reg },
    /// `SetIndex` with a constant key.
    SetIndexK { obj: Reg, k: u32, val: Reg },
    /// Where a failed key computation of a write lands: a dictionary takes
    /// the property's name (`name`) as the key, anything else ignores it.
    SetIndexFail { obj: Reg, val: Reg, name: u32, key: u32, skip: u32 },
    /// Before a push/pop: the target must be a list and not a constant.
    ListCheck { list: Reg, target: u32 },
    ListPush { list: Reg, val: Reg, front: bool, target: u32 },
    ListPop { dst: Reg, list: Reg, front: bool },
    /// After a push onto `obj`'s field `name`: the new element must fit the
    /// field's declared type, else it is taken back off.
    CheckFieldPush { list: Reg, obj: Reg, name: u32, front: bool },

    /// A value as text (template parts).
    Format { dst: Reg, src: Reg },
    Concat { dst: Reg, base: Reg, n: u16 },
    /// `<이름>` as a value.
    FuncRef { dst: Reg, name: u32, var: u32 },
    /// Something this version cannot run yet (named by constant `k`).
    Unsupported { k: u32 },
    /// Raises the error code in constant `k` with string constants as arguments.
    Fail { k: u32, args: [u32; 3] },

    // ---- classes (names are symbols)
    /// `새로운 [클래스]`: a new object, after checking the class can be made.
    NewObj { dst: Reg, class: u32 },
    /// A field initializer of the class being made.
    InitField { obj: Reg, name: u32, src: Reg, ty: u32 },
    /// Runs a constructor on `obj` (its result is dropped).
    CallCtor { obj: Reg, proto: u32, class: u32, base: Reg, argc: u16 },
    /// Ends a `새로운` without a constructor (the call nesting count).
    Leave,
    /// `나`: the running method's object.
    GetThis { dst: Reg },
    /// `'나'`: the object when in a method, else a variable of that name.
    SelfOr { dst: Reg, var: u32 },
    /// `우리`: the running method's class.
    GetStatic { dst: Reg },
    /// `'우리'`: that class when in a method, else a variable of that name.
    StaticOr { dst: Reg, var: u32 },
    /// `[이름]` as a value: a variable of that name, a class, or the name.
    TypeValue { dst: Reg, var: u32, name: u32 },
    InstanceOf { dst: Reg, a: Reg, b: Reg },
    /// Before a method call's arguments: the checks Hana makes when it
    /// evaluates the callee (access, a static method's existence, the type).
    MethodPrep { obj: Reg, name: u32 },
    /// `부모의 ...` needs a method's object and a method name.
    SuperPrep { method: bool },
    CallSuper { dst: Reg, name: u32, base: Reg, argc: u16 },
    /// A static variable of `class` (NONE: the running method's class).
    SetStatic { class: u32, name: u32, src: Reg },

    // ---- errors
    Throw { src: Reg },
    /// Jumps when the error held for handler `key` is an object of `class`.
    CatchIs { key: u32, class: u32, to: u32 },
    /// Puts the error held for `key` into a variable, as an error object.
    CatchBind { key: u32, slot: Reg },
    /// Raises again what handler `key` holds.
    Rethrow { key: u32 },
    /// A `돌려주자` that must pass `마무리는 항상` blocks (or is at the top level).
    ReturnSignal { src: Reg },

    // ---- modules
    /// Runs a `가져오자` (see [`ImportInfo`]): loads the module once, then
    /// binds the names here.
    Import { import: u32 },
    /// `새로운` for a class the compiling module does not declare: its field
    /// initializers, then its constructor, as found when it runs.
    InitFieldsDyn { obj: Reg },
    CallCtorDyn { obj: Reg, base: Reg, argc: u16 },
    /// `<'변수'>`: the function name a variable holds, decided once per site
    /// (Hana rewrites the call site the first time).
    Reflect { dst: Reg, site: u32, var: u32, quoted: u32 },
    /// A method call whose name is in a register (reflection).
    CallMethodDyn { dst: Reg, obj: Reg, name: Reg, base: Reg, argc: u16 },
    MethodPrepDyn { obj: Reg, name: Reg },
}

/// A loop's code range; a break arriving there leaves to `exit`.
#[derive(Clone, Copy, Debug)]
pub struct LoopRange {
    pub start: u32,
    pub end: u32,
    pub exit: u32,
}

/// Code whose errors (and, by kind, breaks and returns) are caught:
/// `start..end` jumps to `target`, which finds what was caught under the
/// handler's index.
#[derive(Clone, Copy, Debug)]
pub struct Handler {
    pub start: u32,
    pub end: u32,
    pub target: u32,
    /// Call expressions open around the range (to restore the nesting count).
    pub open_calls: u32,
    pub kind: HandlerKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandlerKind {
    /// A member key being computed: errors and breaks (Hana swallows both).
    Protect,
    /// `오류가 발생했다면`: errors only.
    Catch,
    /// `마무리는 항상`: errors, breaks and returns.
    Finally,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: u32,
    pub slot: Reg,
    pub meta: Option<Reg>,
    /// Declared type id (0: none).
    pub ty: u32,
}

pub struct Proto {
    pub name: String,
    /// The module whose code this is.
    pub module: u32,
    /// Binds only the first argument, without checks (`<기호 같다>` as `==`).
    pub raw_params: bool,
    pub code: Vec<Op>,
    pub nregs: Reg,
    pub params: Vec<Param>,
    /// Where the body starts after the parameter prologue: a call that
    /// passes every argument starts there (each `ArgGiven` would jump on).
    pub body: u32,
    pub return_type: u32,
    pub loops: Vec<LoopRange>,
    pub handlers: Vec<Handler>,
    /// What printing the function as a value shows (Hana's `String()` of
    /// its declaration).
    pub text: std::rc::Rc<str>,
}

/// A parsed type annotation: `숫자`, `(숫자)목록`, `(문자열, 숫자)사전`,
/// resolved against the language's type names once, at compile time.
#[derive(Clone, Debug)]
pub struct TypeSpec {
    /// As written (for messages).
    pub text: String,
    pub kind: TypeKind,
}

/// What a type accepts. Element types are single names (Hana does not parse
/// nested generics: `((숫자)목록)목록`'s element is a class named `(숫자)목록`).
#[derive(Clone, Debug, PartialEq)]
pub enum TypeKind {
    Any,
    Number,
    String,
    Boolean,
    /// `[비어있음]`: only null fits (every type accepts null).
    Null,
    List(Option<Box<TypeKind>>),
    /// Key and value types; `(숫자)사전` constrains only values.
    Dict(Option<(Box<TypeKind>, Box<TypeKind>)>),
    /// A class or interface, by symbol.
    Class(u32),
}

impl TypeSpec {
    pub fn parse(text: &str, lang: &Lang) -> TypeSpec {
        let t = text.trim();
        let (name, args): (&str, Vec<&str>) = match t.strip_prefix('(').and_then(|rest| rest.find(')').filter(|&e| e > 0).map(|e| (rest, e))) {
            Some((rest, end)) => (rest[end + 1..].trim(), rest[..end].split(',').map(str::trim).filter(|a| !a.is_empty()).collect()),
            None => (t, Vec::new()),
        };
        let kind = match TypeKind::named(name, lang) {
            TypeKind::List(_) => TypeKind::List(args.first().map(|a| Box::new(TypeKind::named(a, lang)))),
            TypeKind::Dict(_) => TypeKind::Dict(match args.as_slice() {
                [] => None,
                [v] => Some((Box::new(TypeKind::Any), Box::new(TypeKind::named(v, lang)))),
                [k, v, ..] => Some((Box::new(TypeKind::named(k, lang)), Box::new(TypeKind::named(v, lang)))),
            }),
            other => other,
        };
        TypeSpec { text: text.to_string(), kind }
    }
}

impl TypeKind {
    fn named(name: &str, l: &Lang) -> TypeKind {
        match name {
            "" => TypeKind::Any,
            n if n == l.type_any => TypeKind::Any,
            n if n == l.type_number => TypeKind::Number,
            n if n == l.type_string => TypeKind::String,
            n if n == l.type_boolean => TypeKind::Boolean,
            n if n == l.type_null => TypeKind::Null,
            n if n == l.type_list => TypeKind::List(None),
            n if n == l.type_dict => TypeKind::Dict(None),
            n => TypeKind::Class(crate::symbol::intern(n)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Public,
    Private,
    Protected,
}

impl Access {
    pub fn parse(s: &str) -> Access {
        match s {
            "private" => Access::Private,
            "protected" => Access::Protected,
            _ => Access::Public,
        }
    }
}

/// A name looked up on a class (Hana's `classMember`): the field and the
/// method of that name, each from the first class of the chain declaring it.
#[derive(Clone, Copy, Debug)]
pub struct Member {
    pub field_access: Access,
    pub getter: Option<u32>,
    /// The setter's body, taking the new value as its only argument.
    pub setter: Option<u32>,
    pub method: Option<u32>,
    pub method_access: Access,
}

impl Default for Member {
    fn default() -> Member {
        Member { field_access: Access::Public, getter: None, setter: None, method: None, method_access: Access::Public }
    }
}

pub struct ClassInfo {
    pub name: u32,
    pub is_abstract: bool,
    pub members: crate::value::Map<u32, Member>,
    /// Declared type of a field (first non-static declaration of the chain).
    pub field_types: crate::value::Map<u32, u32>,
    /// The constructor (first of the chain).
    pub ctor: Option<u32>,
    /// Static methods of the class itself.
    pub statics: crate::value::Map<u32, u32>,
    /// `<기호 같다>` of the class itself, compiled for `==`.
    pub equals: Option<u32>,
    /// The method each other operator calls (`<기호 더하기>` …, found as a
    /// method call finds it), by `BinOp`.
    pub operators: [Option<u32>; 9],
    /// Its own field initializers as a body (see `Op::InitFieldsDyn`).
    pub init: u32,
    /// The class and its ancestors (instanceof, catch types).
    pub lineage: Vec<u32>,
    /// Types a value of this class passes as (ancestors and every interface
    /// they declare).
    pub supertypes: HashSet<u32>,
    /// Where `부모의` starts: None without a parent (the class itself), Some(None)
    /// when the parent is not a class.
    pub super_start: Option<Option<u32>>,
}

pub struct Program {
    /// The language of the program's own file.
    pub lang: &'static Lang,
    pub protos: Vec<Proto>,
    pub consts: Vec<Value>,
    pub vars: Vec<Var>,
    /// `types[0]` is unused (0 means "no type").
    pub types: Vec<TypeSpec>,
    /// Initial values of the globals of every module (built-ins are defined;
    /// the rest undefined).
    pub globals: Vec<Value>,
    /// `modules[0]` is the program itself; the rest are the files it imports.
    pub modules: Vec<ModuleInfo>,
    /// Every class declaration of every module (a class's id is its index).
    pub classes: Vec<ClassInfo>,
    pub imports: Vec<ImportInfo>,
}

/// A source file: the program or a file it imports. Each runs in its own
/// namespace (Hana's sub-interpreter): its own globals, functions and classes.
pub struct ModuleInfo {
    /// `file:<path as written>` (Hana's cache key), or `<main>`.
    pub key: String,
    pub lang: &'static Lang,
    /// Its top-level code.
    pub main: u32,
    /// Its global variables by name, and the range of global slots it owns.
    pub globals: HashMap<String, u32>,
    pub global_range: (u32, u32),
    /// Its top-level functions: the first of each name (for calls), and all
    /// in order (for `전부`, where a later one of a name wins).
    pub functions: HashMap<String, u32>,
    pub all_functions: Vec<(String, u32)>,
    /// Its classes (name symbol -> class id) and interfaces.
    pub classes: crate::value::Map<u32, u32>,
    pub interfaces: HashSet<u32>,
    /// The name of its language's built-in error class.
    pub error_class: u32,
}

pub enum ImportKind {
    /// A file module.
    File(u32),
    /// A standard module or a package.
    Package(PackageImport),
    /// An import that fails when it runs (a missing or unreadable file...).
    Fail { code: String, args: Vec<String> },
}

/// A `[모듈]` import, resolved the way Hana resolves one: `<네이티브_이름>`
/// items from its native module, the rest from its source entry point for
/// the language, or else from the native module's own names.
pub struct PackageImport {
    /// Its native module in the runtime, or the error a native item meets.
    pub native: Result<usize, (String, Vec<String>)>,
    /// A standard module (only those can be listed by `전부` in Hana).
    pub core: bool,
    pub source: PackageSource,
}

pub enum PackageSource {
    /// The compiled entry point for the importer's language.
    Module(u32),
    /// There is one, but it cannot be used (another language only, a syntax error...).
    Fail(String, Vec<String>),
    None,
}

/// One `가져오자` statement.
pub struct ImportInfo {
    pub kind: ImportKind,
    /// The module as written (for messages).
    pub source: String,
    pub all: bool,
    /// (name in the module, name here, where it goes here).
    pub items: Vec<(String, String, Loc)>,
    /// Where the names `전부` brings go here.
    pub all_slots: HashMap<String, Loc>,
    /// Classes the statement renames (their conflicts are not errors).
    pub aliased: HashSet<u32>,
    /// Native functions the module has as variables, which come along with
    /// any import of it (Hana's `injectNatives`): (name, where it goes here).
    pub leaks: Vec<(String, Loc)>,
}
