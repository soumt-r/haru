//! The compiled form of a program: register-machine code per function.
//!
//! Every variable is resolved at compile time to the slots it can live in
//! (see [`Var`]); nothing is looked up by name while running.

use crate::lang::Lang;
use crate::value::Value;

pub type Reg = u16;

/// Where a variable lives: a register of the running function's frame, or a
/// global of the program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Loc {
    Reg(Reg),
    Global(u32),
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
    /// Marks registers `from..to` as holding no variable (a new loop pass).
    Undef { from: Reg, to: Reg },

    Bin { op: BinOp, dst: Reg, a: Reg, b: Reg },
    /// `Bin` with a number constant on the right.
    BinK { op: BinOp, dst: Reg, a: Reg, k: u32 },
    /// `==` (or `!=` when `neg`).
    Eq { dst: Reg, a: Reg, b: Reg, neg: bool },
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
    /// Where a failed key computation lands (a list or string reports its own error).
    IndexFail { obj: Reg },
    /// A member write, first half: strings refuse, other values ignore it.
    SetMember { obj: Reg, skip: u32 },
    SetIndex { obj: Reg, key: Reg, val: Reg },
    /// Where a failed key computation of a write lands: a dictionary takes
    /// the property's name (`name`) as the key, anything else ignores it.
    SetIndexFail { obj: Reg, val: Reg, name: u32, skip: u32 },
    /// Before a push/pop: the target must be a list and not a constant.
    ListCheck { list: Reg, target: u32 },
    ListPush { list: Reg, val: Reg, front: bool, target: u32 },
    ListPop { dst: Reg, list: Reg, front: bool },

    /// A value as text (template parts).
    Format { dst: Reg, src: Reg },
    Concat { dst: Reg, base: Reg, n: u16 },
    /// `<이름>` as a value.
    FuncRef { dst: Reg, name: u32, var: u32 },
    /// Something this version cannot run yet (named by constant `k`).
    Unsupported { k: u32 },
}

/// A loop's code range; a break arriving there leaves to `exit`.
#[derive(Clone, Copy, Debug)]
pub struct LoopRange {
    pub start: u32,
    pub end: u32,
    pub exit: u32,
}

/// Code whose errors are caught: `start..end` jumps to `target`.
#[derive(Clone, Copy, Debug)]
pub struct Handler {
    pub start: u32,
    pub end: u32,
    pub target: u32,
    /// Call expressions open around the range (to restore the nesting count).
    pub open_calls: u32,
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
    pub code: Vec<Op>,
    pub nregs: Reg,
    pub params: Vec<Param>,
    pub return_type: u32,
    pub loops: Vec<LoopRange>,
    pub handlers: Vec<Handler>,
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
    Class(String),
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
            n => TypeKind::Class(n.to_string()),
        }
    }
}

pub struct Program {
    pub lang: &'static Lang,
    /// `protos[0]` is the program's top level.
    pub protos: Vec<Proto>,
    pub consts: Vec<Value>,
    pub names: Vec<String>,
    pub vars: Vec<Var>,
    /// `types[0]` is unused (0 means "no type").
    pub types: Vec<TypeSpec>,
    /// Initial values of the globals (built-ins are defined; the rest undefined).
    pub globals: Vec<Value>,
    /// Global variables by name, for names only known at run time.
    pub global_names: Vec<(String, u32)>,
    /// Top-level functions by name.
    pub functions: Vec<(String, u32)>,
}
