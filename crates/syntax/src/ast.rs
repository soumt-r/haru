//! The syntax tree. Node and field names follow Hana's `ast` package one to
//! one (the JSON dump in `dump.rs` uses the Go names), so the two parsers can
//! be compared tree for tree.

#[derive(Clone, Debug, PartialEq)]
pub struct Program {
    pub statements: Vec<Stmt>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Block {
    pub statements: Vec<Stmt>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypeRef {
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Identifier(String),
    SelfRef,
    SuperRef,
    StaticRef,
    Number(f64),
    /// The raw text between the quotes (escapes are processed by the compiler).
    Str(String),
    Null,
    FunctionRef(String),
    TypeRef(TypeRef),
    Member { object: Box<Expr>, property: Box<Expr> },
    Call { callee: Box<Expr>, args: Vec<Expr> },
    New { class: Option<TypeRef>, args: Vec<Expr> },
    Binary { left: Box<Expr>, op: String, right: Box<Expr> },
    /// `그리고` / `또는` (the operator is kept in Korean, as in Hana).
    Logical { left: Box<Expr>, op: String, right: Box<Expr> },
    List(Vec<Expr>),
    Dict(Vec<(Expr, Expr)>),
    ListPop { target: Box<Expr>, position: &'static str },
    /// The raw template text; `{...}` parts are parsed by the compiler.
    Template(String),
    Bool(bool),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub name: String,
    pub type_annotation: Option<TypeRef>,
    pub default: Option<Expr>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Setter {
    pub param: Option<String>,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VarDecl {
    pub name: Option<String>,
    pub type_ref: Option<TypeRef>,
    pub value: Option<Expr>,
    pub is_constant: bool,
    pub access: &'static str,
    pub is_static: bool,
    /// `None` when there is no getter; `Some(empty)` for an empty one.
    pub getter: Option<Vec<Stmt>>,
    pub setter: Option<Setter>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SwitchCase {
    pub tests: Vec<Expr>,
    pub consequent: Block,
    pub is_default: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CatchClause {
    pub type_ref: Option<TypeRef>,
    pub param: String,
    pub body: Block,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImportItem {
    pub name: String,
    /// Empty unless renamed (`<이름>을 <별칭>으로`).
    pub alias: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FuncDecl {
    pub name: String,
    pub params: Vec<Param>,
    pub body: Block,
    pub access: &'static str,
    pub is_static: bool,
    pub return_type: Option<TypeRef>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stmt {
    VarDecl(VarDecl),
    Assign { target: Expr, value: Option<Expr> },
    Print { value: Expr, newline: bool },
    Input { target: Option<String>, type_ref: Option<TypeRef> },
    Expr(Expr),
    Switch { discriminant: Expr, cases: Vec<SwitchCase> },
    Fallthrough,
    If { condition: Expr, consequent: Block, alternate: Option<Block> },
    Return(Option<Expr>),
    Break,
    ForEach { list: Expr, body: Block },
    ForRange { start: Expr, end: Expr, loop_var: String, body: Block },
    While { condition: Expr, body: Block },
    Class {
        name: Option<TypeRef>,
        base: Option<TypeRef>,
        interfaces: Vec<TypeRef>,
        body: Vec<Stmt>,
        is_abstract: bool,
    },
    Import { module: String, is_builtin: bool, all: bool, items: Vec<ImportItem> },
    Interface { name: Option<TypeRef>, body: Vec<Stmt> },
    ListPush { target: Expr, value: Expr, position: &'static str },
    ListPop { target: Expr, position: &'static str },
    Try { block: Block, handlers: Vec<CatchClause>, finalizer: Option<Block> },
    Throw(Expr),
    Function(FuncDecl),
    InterfaceMethod(String),
    Constructor { id: String, params: Vec<Param>, body: Vec<Stmt> },
}
