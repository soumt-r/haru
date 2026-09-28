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
    /// Where the name is written (for the parser's diagnostics; not syntax).
    pub src: Src,
}

/// A place in the source: line, 0-based column and length in characters.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Src {
    pub line: u32,
    pub col: u32,
    pub len: u32,
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
    /// `src` is where it starts (for the parser's diagnostics; not syntax).
    Constructor { id: String, params: Vec<Param>, body: Vec<Stmt>, src: Src },
}

// Hana's `String()` of each node: what Hana prints for a function value is
// its declaration's text in this form (`func 이름() {` and one line per
// statement), so Haru keeps the same words.

impl Expr {
    pub fn go_string(&self) -> String {
        match self {
            Expr::Identifier(s) | Expr::Template(s) => s.clone(),
            Expr::SelfRef => "SelfReference".into(),
            Expr::SuperRef => "SuperReference".into(),
            Expr::StaticRef => "StaticReference".into(),
            Expr::Number(_) => "NumberLiteral".into(),
            Expr::Str(s) => format!("\"{s}\""),
            Expr::Null => "Null".into(),
            Expr::FunctionRef(s) => format!("<{s}>"),
            Expr::TypeRef(t) => format!("[{}]", t.name),
            Expr::Member { object, property } => format!("{}.{}", object.go_string(), property.go_string()),
            Expr::Call { callee, .. } => format!("{}(...)", callee.go_string()),
            Expr::New { class, .. } => format!("New [{}]()", class.as_ref().map_or("", |c| &c.name)),
            Expr::Binary { left, op, right } | Expr::Logical { left, op, right } => {
                format!("{} {} {}", left.go_string(), op, right.go_string())
            }
            Expr::List(_) => "[List]".into(),
            Expr::Dict(_) => "{Dict}".into(),
            Expr::ListPop { .. } => "ListPopExpr".into(),
            Expr::Bool(b) => if *b { "true" } else { "false" }.into(),
        }
    }
}

fn go_opt(e: &Option<Expr>) -> String {
    e.as_ref().map_or(String::new(), Expr::go_string)
}

fn go_body(head: String, body: &[Stmt]) -> String {
    let mut out = head;
    for s in body {
        out.push_str("  ");
        out.push_str(&s.go_string());
        out.push('\n');
    }
    out.push('}');
    out
}

impl Stmt {
    pub fn go_string(&self) -> String {
        match self {
            Stmt::VarDecl(v) => {
                let name = v.name.as_deref().unwrap_or("");
                match &v.value {
                    None => format!("var {name}"),
                    Some(e) => format!("var {name} = {}", e.go_string()),
                }
            }
            Stmt::Assign { target, value } => format!("{} = {}", target.go_string(), go_opt(value)),
            Stmt::Print { value, .. } => format!("Print({})", value.go_string()),
            Stmt::Input { target, .. } => format!("Input({})", target.as_deref().unwrap_or("")),
            Stmt::Expr(e) => e.go_string(),
            Stmt::Switch { .. } => "switch".into(),
            Stmt::Fallthrough => "fallthrough".into(),
            Stmt::If { condition, .. } => format!("If {}", condition.go_string()),
            Stmt::Return(None) => "Return".into(),
            Stmt::Return(Some(e)) => format!("Return {}", e.go_string()),
            Stmt::Break => "Break".into(),
            Stmt::ForEach { list, .. } => format!("ForEach {}", list.go_string()),
            Stmt::ForRange { start, end, .. } => format!("ForRange {} to {}", start.go_string(), end.go_string()),
            Stmt::While { condition, .. } => format!("While {}", condition.go_string()),
            Stmt::Class { name, base, body, .. } => {
                let name = name.as_ref().map_or("", |t| &t.name);
                let head = match base {
                    Some(b) => format!("class {name} extends {} {{\n", b.name),
                    None => format!("class {name} {{\n"),
                };
                go_body(head, body)
            }
            Stmt::Import { module, is_builtin, all, items } => {
                let what = if *all {
                    "*".to_string()
                } else {
                    let names: Vec<String> = items
                        .iter()
                        .map(|i| if i.alias.is_empty() { i.name.clone() } else { format!("{} as {}", i.name, i.alias) })
                        .collect();
                    names.join(", ")
                };
                if *is_builtin {
                    format!("Import {what} from builtin {module}")
                } else {
                    format!("Import {what} from {module}")
                }
            }
            Stmt::Interface { name, body } => {
                go_body(format!("interface [{}] {{\n", name.as_ref().map_or("", |t| &t.name)), body)
            }
            Stmt::ListPush { .. } => "ListPush".into(),
            Stmt::ListPop { .. } => "ListPop".into(),
            Stmt::Try { .. } => "Try".into(),
            Stmt::Throw(_) => "Throw".into(),
            Stmt::Function(f) => f.go_string(),
            Stmt::InterfaceMethod(name) => format!("{name}()"),
            Stmt::Constructor { .. } => "Constructor".into(),
        }
    }
}

impl FuncDecl {
    pub fn go_string(&self) -> String {
        go_body(format!("func {}() {{\n", self.name), &self.body.statements)
    }
}
