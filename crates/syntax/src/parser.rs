//! The parser: one implementation for every language, driven by a
//! [`Profile`]. It is a faithful port of Hana's `parser/hari` — the grammar is
//! defined by that parser's behaviour, quirks included, so any change here must
//! keep `haru ast-check` (tree-for-tree comparison with Hana) passing.
//!
//! Where Hana would panic on malformed input (reading past the end), this
//! parser reads EOF tokens instead and reports a diagnostic.

use crate::ast::*;
use crate::profile::{LoopKind, Profile};
use crate::token::{Kind, Token};

/// One part of a verb-final sentence: an expression and the particles after it.
pub struct Component {
    pub expr: Expr,
    pub particles: Vec<String>,
    /// The `[타입]인 값` annotation, when the component had one.
    pub type_ref: Option<TypeRef>,
}

/// A parse problem. An empty `literal` means the parser ran off the end.
#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub line: u32,
    /// 0-based, in characters.
    pub col: u32,
    pub length: u32,
    pub literal: String,
}

pub struct Parser<'a> {
    tokens: Vec<Token<'a>>,
    pos: usize,
    lang: &'static Profile,
    diags: Vec<Diagnostic>,
    declared_type: Option<TypeRef>,
}

fn is_verb(k: Kind) -> bool {
    use Kind::*;
    matches!(
        k,
        KwMake
            | KwExecute
            | KwPrint
            | KwReturn
            | KwLoop
            | KwPush
            | KwPop
            | KwThrow
            | KwMustHave
            | KwImport
            | KwAdd
            | KwSub
            | KwSwitch
            | KwFallthrough
            | KwInput
    )
}

/// Whether a token can follow a complete condition.
fn ends_condition(k: Kind) -> bool {
    matches!(k, Kind::RParen | Kind::Colon | Kind::KwAnd | Kind::KwOr | Kind::Ident)
}

/// `s` without `n` bytes at each end (empty when too short).
fn strip(s: &str, n: usize) -> String {
    if s.len() < 2 * n {
        return String::new();
    }
    s.get(n..s.len() - n).unwrap_or("").to_string()
}

fn import_name(e: &Expr) -> String {
    match e {
        Expr::Identifier(s) | Expr::Str(s) | Expr::FunctionRef(s) => s.clone(),
        _ => String::new(),
    }
}

impl<'a> Parser<'a> {
    pub fn new(tokens: Vec<Token<'a>>, lang: &'static Profile) -> Parser<'a> {
        Parser { tokens, pos: 0, lang, diags: Vec::new(), declared_type: None }
    }

    /// What to report. Running off the end is usually fallout of an earlier
    /// bad token, so it is kept only when nothing else explains it, and once.
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        let real: Vec<Diagnostic> = self.diags.iter().filter(|d| !d.literal.is_empty()).cloned().collect();
        if real.is_empty() && !self.diags.is_empty() {
            return vec![self.diags[0].clone()];
        }
        real
    }

    fn peek(&self, offset: usize) -> Option<Token<'a>> {
        self.tokens.get(self.pos + offset).copied()
    }

    fn kind(&self, offset: usize) -> Option<Kind> {
        self.peek(offset).map(|t| t.kind)
    }

    fn at(&self, k: Kind) -> bool {
        self.kind(0) == Some(k)
    }

    fn lit(&self, offset: usize) -> Option<&'a str> {
        self.peek(offset).map(|t| t.lit)
    }

    fn consume(&mut self) -> Token<'a> {
        let t = self.tokens.get(self.pos).copied().unwrap_or_else(|| Token {
            kind: Kind::Eof,
            lit: "",
            line: self.tokens.last().map_or(1, |t| t.line),
            col: 0,
        });
        self.pos += 1;
        t
    }

    fn skip_until_colon(&mut self) {
        while self.peek(0).is_some() && !self.at(Kind::Colon) {
            self.consume();
        }
    }

    fn last_content_line(&self) -> u32 {
        self.tokens
            .iter()
            .rev()
            .find(|t| !matches!(t.kind, Kind::Eof | Kind::Indent | Kind::Dedent))
            .map_or(1, |t| t.line)
    }

    pub fn parse_program(&mut self) -> Program {
        let mut statements = Vec::new();
        while let Some(t) = self.peek(0) {
            if t.kind == Kind::Eof {
                break;
            }
            if matches!(t.kind, Kind::Indent | Kind::Dedent) {
                self.consume();
                continue;
            }
            if let Some(s) = self.parse_statement() {
                statements.push(s);
            }
        }
        Program { statements }
    }

    fn parse_block(&mut self) -> Block {
        let mut block = Block::default();
        if self.at(Kind::Colon) {
            self.consume();
        }
        if self.at(Kind::Indent) {
            self.consume();
            while let Some(k) = self.kind(0) {
                if k == Kind::Dedent || k == Kind::Eof {
                    break;
                }
                if let Some(s) = self.parse_statement() {
                    block.statements.push(s);
                }
            }
            if self.at(Kind::Dedent) {
                self.consume();
            }
        }
        block
    }

    fn parse_statement(&mut self) -> Option<Stmt> {
        let tok = self.peek(0)?;
        match tok.kind {
            Kind::Eof => return None,
            Kind::Indent => {
                self.consume();
                return None;
            }
            Kind::Dedent => return None,
            _ => {}
        }

        if tok.kind == Kind::LBracket || tok.kind == Kind::Type {
            // A class or interface declaration, if its verb is on this line.
            for t in &self.tokens[self.pos..] {
                if t.line != tok.line || matches!(t.kind, Kind::Colon | Kind::KwFrom | Kind::KwImport) {
                    break;
                }
                if matches!(t.kind, Kind::KwClass | Kind::KwImplements) {
                    return Some(self.parse_class());
                }
                if t.kind == Kind::KwInterface {
                    return Some(self.parse_interface());
                }
            }
        }

        let mut is_func_decl = false;
        for i in self.pos..self.tokens.len() {
            let t = self.tokens[i];
            if t.line != tok.line || matches!(t.kind, Kind::Colon | Kind::Dedent | Kind::Indent) {
                break;
            }
            if t.kind == Kind::Function && i + 1 < self.tokens.len() {
                let mut next = i + 1;
                if self.tokens[next].kind == Kind::Particle {
                    next += 1;
                }
                if next < self.tokens.len() && matches!(self.tokens[next].kind, Kind::KwMake | Kind::KwMustHave) {
                    is_func_decl = true;
                    break;
                }
            }
        }
        if is_func_decl {
            return Some(self.parse_function_decl());
        }

        match tok.kind {
            Kind::KwIf => return Some(self.parse_if()),
            Kind::KwConstruct => return Some(self.parse_constructor()),
            Kind::KwBreak => {
                self.consume();
                return Some(Stmt::Break);
            }
            Kind::KwReturn => {
                self.consume();
                return Some(Stmt::Return(None));
            }
            Kind::KwTry => return Some(self.parse_try()),
            _ => {}
        }

        // A property with getter/setter: `... 정하자:` on this line.
        let mut i = 0;
        while let Some(t) = self.peek(i) {
            if t.line != tok.line || t.kind == Kind::Eof {
                break;
            }
            if t.kind == Kind::Colon && i > 0 && self.kind(i - 1) == Some(Kind::KwMake) {
                return Some(self.parse_property_declaration());
            }
            i += 1;
        }

        self.parse_generic_sov()
    }

    fn parse_property_declaration(&mut self) -> Stmt {
        let name = match self.parse_primary() {
            Expr::Identifier(n) => Some(n),
            _ => None,
        };
        if self.at(Kind::Particle) {
            self.consume();
        }
        let type_ref = self.parse_type_ref();
        if self.at(Kind::Particle) {
            self.consume();
        }
        self.consume(); // 정하자
        self.consume(); // :
        if self.at(Kind::Indent) {
            self.consume();
        }

        let mut getter = None;
        let mut setter = None;
        while let Some(k) = self.kind(0) {
            if k == Kind::Dedent || k == Kind::Eof {
                break;
            }
            if k == Kind::KwGetter {
                self.consume(); // 가져올 때
                self.consume(); // :
                getter = Some(self.parse_block().statements);
            } else if k == Kind::KwSetter {
                self.consume(); // 정할 때
                let mut param = None;
                if self.at(Kind::LParen) {
                    self.consume();
                    if let Expr::Identifier(n) = self.parse_primary() {
                        param = Some(n);
                    }
                    self.consume(); // )
                }
                self.consume(); // :
                setter = Some(Setter { param, body: self.parse_block().statements });
            } else {
                self.consume();
            }
        }
        if self.at(Kind::Dedent) {
            self.consume();
        }
        Stmt::VarDecl(VarDecl {
            name,
            type_ref,
            value: None,
            is_constant: false,
            access: "public",
            is_static: false,
            getter,
            setter,
        })
    }

    fn parse_try(&mut self) -> Stmt {
        self.consume(); // 일단 해보자
        self.skip_until_colon();
        let block = self.parse_block();

        let mut handlers = Vec::new();
        while let Some(t) = self.peek(0) {
            // If this turns out not to be a catch clause, give back what was read.
            let start = self.pos;
            let mut err_type = None;
            if t.kind == Kind::Type {
                err_type = self.parse_type_ref();
            } else if t.kind == Kind::Ident && self.lang.error_literals.contains(&t.lit) {
                self.consume();
            } else if t.kind != Kind::KwCatch {
                break;
            }
            if self.at(Kind::Particle) {
                self.consume();
            }
            if !self.at(Kind::KwCatch) {
                self.pos = start;
                break;
            }
            self.consume(); // 발생했다면

            let mut param = String::new();
            if self.at(Kind::LParen) {
                self.consume();
                if matches!(self.kind(0), Some(Kind::Var | Kind::Ident)) {
                    let name = self.consume();
                    param = if name.kind == Kind::Var { strip(name.lit, self.lang.delim_len) } else { name.lit.to_string() };
                }
                if self.at(Kind::RParen) {
                    self.consume();
                }
            } else {
                param = "에러".to_string();
            }
            self.skip_until_colon();
            let body = self.parse_block();
            handlers.push(CatchClause { type_ref: err_type, param, body });
        }

        let mut finalizer = None;
        if self.at(Kind::KwFinally) {
            self.consume();
            self.skip_until_colon();
            finalizer = Some(self.parse_block());
        }
        Stmt::Try { block, handlers, finalizer }
    }

    fn type_name(&self, lit: &str) -> String {
        if lit.starts_with(self.lang.type_open) && lit.ends_with(self.lang.type_close) {
            strip(lit, self.lang.delim_len)
        } else {
            lit.to_string()
        }
    }

    fn parse_type_ref(&mut self) -> Option<TypeRef> {
        match self.kind(0)? {
            Kind::Type => {
                let t = self.consume();
                Some(TypeRef { name: self.type_name(t.lit) })
            }
            Kind::LBracket => {
                self.consume(); // [
                let name = self.consume().lit.to_string();
                self.consume(); // ]
                Some(TypeRef { name })
            }
            _ => None,
        }
    }

    fn parse_interface(&mut self) -> Stmt {
        let name = self.parse_type_ref();
        if self.at(Kind::Particle) {
            self.consume();
        }
        self.consume(); // 규정하자
        let block = self.parse_block();
        Stmt::Interface { name, body: block.statements }
    }

    fn parse_class(&mut self) -> Stmt {
        let mut base = None;
        let mut interfaces = Vec::new();
        let mut name = None;
        let mut is_abstract = false;

        while let Some(k) = self.kind(0) {
            if matches!(k, Kind::Colon | Kind::Dedent | Kind::Indent) {
                break;
            }
            if k == Kind::Type || k == Kind::LBracket {
                let t = self.parse_type_ref();
                if self.at(Kind::Particle) {
                    self.consume();
                }
                match self.kind(0) {
                    Some(Kind::KwBase) => {
                        self.consume();
                        base = t;
                    }
                    Some(Kind::KwImplements) => {
                        self.consume();
                        interfaces.extend(t);
                    }
                    Some(Kind::KwClass) => {
                        name = t;
                        is_abstract = self.consume().lit.starts_with(self.lang.abstract_prefix);
                        break;
                    }
                    _ => name = t,
                }
            } else if k == Kind::KwClass {
                is_abstract = self.consume().lit.starts_with(self.lang.abstract_prefix);
                break;
            } else {
                self.consume();
            }
        }

        self.skip_until_colon();
        if self.at(Kind::Colon) {
            self.consume();
        }
        let block = self.parse_block();
        Stmt::Class { name, base, interfaces, body: block.statements, is_abstract }
    }

    fn parse_function_decl(&mut self) -> Stmt {
        let mut return_type = None;
        let mut is_static = false;
        while let Some(t) = self.peek(0) {
            if t.kind == Kind::Function || t.kind == Kind::Colon {
                break;
            }
            if t.kind == Kind::Type || t.kind == Kind::LBracket {
                return_type = self.parse_type_ref();
            } else {
                if matches!(t.kind, Kind::Var | Kind::Ident) && self.lang.plural_self_words.contains(&t.lit) {
                    is_static = true;
                }
                self.consume();
            }
        }

        let name = strip(self.consume().lit, self.lang.delim_len);
        if self.at(Kind::Particle) {
            self.consume();
        }
        let action = self.consume();

        if self.at(Kind::LParen) {
            self.consume();
        }
        let params = self.parse_params(true);
        if self.at(Kind::RParen) {
            self.consume();
        }

        if action.kind == Kind::KwMustHave {
            return Stmt::InterfaceMethod(name);
        }
        let access = self.lang.access_modifier(action.lit);
        let body = self.parse_block();
        Stmt::Function(FuncDecl { name, params, body, access, is_static, return_type })
    }

    /// Parameters up to `)`. Function declarations accept bare identifiers and
    /// skip particles; constructors take only quoted names.
    fn parse_params(&mut self, function: bool) -> Vec<Param> {
        let mut params = Vec::new();
        while let Some(k) = self.kind(0) {
            if k == Kind::RParen {
                break;
            }
            let mut type_annotation = None;
            if k == Kind::LBracket || k == Kind::Type {
                type_annotation = self.parse_type_ref();
                if self.at(Kind::TypeIn) || self.lit(0) == Some(self.lang.type_in_word) {
                    self.consume();
                }
            }

            let mut name = None;
            let named = if function { matches!(self.kind(0), Some(Kind::Var | Kind::Ident)) } else { self.at(Kind::Var) };
            if named {
                if let Expr::Identifier(n) = self.parse_primary() {
                    name = Some(n);
                }
            } else if function && self.at(Kind::Particle) {
                self.consume();
                continue;
            } else {
                self.consume();
            }

            if let Some(name) = name {
                let mut default = None;
                if self.at(Kind::Assign) {
                    self.consume();
                    default = Some(self.parse_expression());
                }
                params.push(Param { name, type_annotation, default });
            }

            if function && (self.lit(0) == Some(",") || self.at(Kind::Particle)) {
                self.consume();
            }
        }
        params
    }

    fn parse_if(&mut self) -> Stmt {
        self.consume(); // 만약
        self.parse_if_body()
    }

    fn parse_if_body(&mut self) -> Stmt {
        let condition = self.parse_condition();
        self.skip_until_colon();
        let consequent = self.parse_block();

        let mut alternate = None;
        if self.at(Kind::KwElse) {
            self.consume();
            if self.at(Kind::KwIf) {
                alternate = Some(Block { statements: vec![self.parse_if()] });
            } else {
                self.skip_until_colon();
                alternate = Some(self.parse_block());
            }
        } else if self.at(Kind::KwElif) {
            self.consume();
            alternate = Some(Block { statements: vec![self.parse_if_body()] });
        }
        Stmt::If { condition, consequent, alternate }
    }

    /// One operand, optionally chained with 그리고/또는, then an optional 라면.
    fn parse_condition(&mut self) -> Expr {
        let mut cond = self.parse_condition_operand();
        while matches!(self.kind(0), Some(Kind::KwAnd | Kind::KwOr)) {
            let op = if self.consume().kind == Kind::KwOr { "또는" } else { "그리고" };
            let right = self.parse_condition_operand();
            cond = Expr::Logical { left: Box::new(cond), op: op.to_string(), right: Box::new(right) };
        }
        if let Some(t) = self.peek(0) {
            if t.kind == Kind::Ident && self.lang.condition_then_words.contains(&t.lit) {
                self.consume();
            }
        }
        cond
    }

    fn parse_condition_operand(&mut self) -> Expr {
        if self.at(Kind::LParen) {
            let (start, diags) = (self.pos, self.diags.len());
            self.consume();
            let cond = self.parse_condition();
            if self.at(Kind::RParen) {
                self.consume();
            }
            match self.kind(0) {
                None => return cond,
                Some(k) if ends_condition(k) => return cond,
                _ => {}
            }
            // The group was the start of an expression (`(('x' % 2) == 0)`): read it again.
            self.pos = start;
            self.diags.truncate(diags);
        }
        let left = self.parse_expression();
        self.finish_comparison(left)
    }

    fn finish_comparison(&mut self, mut cond: Expr) -> Expr {
        if self.at(Kind::Particle) {
            self.consume();
        }
        match self.kind(0) {
            Some(k)
                if !matches!(
                    k,
                    Kind::Compare | Kind::RParen | Kind::Colon | Kind::Ident | Kind::KwAnd | Kind::KwOr
                ) =>
            {
                // SOV: left right compare
                let right = self.parse_expression();
                if self.at(Kind::Particle) {
                    self.consume();
                }
                if self.at(Kind::Compare) {
                    let op = (self.lang.compare_sov)(self.consume().lit);
                    cond = Expr::Binary { left: Box::new(cond), op, right: Box::new(right) };
                }
            }
            Some(Kind::Compare) => {
                // SVO: left compare right
                let op = (self.lang.compare_svo)(self.consume().lit);
                let right = self.parse_expression();
                cond = Expr::Binary { left: Box::new(cond), op, right: Box::new(right) };
            }
            _ => {}
        }
        cond
    }

    fn list_position(&self, particles: &[String]) -> &'static str {
        match particles.first() {
            Some(p) if p.contains(self.lang.front_marker) => "front",
            _ => "back",
        }
    }

    fn parse_generic_sov(&mut self) -> Option<Stmt> {
        let mut components: Vec<Component> = Vec::new();
        while let Some(t) = self.peek(0) {
            if is_verb(t.kind) {
                break;
            }
            self.declared_type = None;
            let expr = self.parse_expression();
            let type_ref = self.declared_type.take();
            let mut particles = Vec::new();
            while let Some(t) = self.peek(0) {
                match t.kind {
                    Kind::Particle | Kind::KwFront | Kind::KwBack => particles.push(t.lit.to_string()),
                    Kind::Comma | Kind::KwFrom | Kind::TypeIn => {}
                    _ => break,
                }
                self.consume();
            }
            components.push(Component { expr, particles, type_ref });
        }

        if matches!(self.kind(0), None | Some(Kind::Eof)) {
            return None;
        }
        let verb = self.consume();
        let first = components.first().map(|c| c.expr.clone());

        match verb.kind {
            Kind::KwAdd | Kind::KwSub => {
                let target = first?;
                let value = components.get(1)?.expr.clone();
                let op = if verb.kind == Kind::KwSub { "-" } else { "+" };
                return Some(Stmt::Assign {
                    target: target.clone(),
                    value: Some(Expr::Binary { left: Box::new(target), op: op.to_string(), right: Box::new(value) }),
                });
            }
            Kind::KwMake => {
                let target = first?;
                let value = components.get(1).map(|c| c.expr.clone());
                let declared = components.get(1).and_then(|c| c.type_ref.clone());
                let access = self.lang.access_modifier(verb.lit);
                let is_constant = verb.lit.ends_with(self.lang.const_suffix);
                let decl = |name: &str, is_constant: bool, is_static: bool, value: Option<Expr>| {
                    Stmt::VarDecl(VarDecl {
                        name: Some(name.to_string()),
                        type_ref: declared.clone(),
                        value,
                        is_constant,
                        access,
                        is_static,
                        getter: None,
                        setter: None,
                    })
                };
                if let Expr::Identifier(id) = &target {
                    return Some(decl(id, is_constant, false, value));
                }
                if let Expr::Member { object, property } = &target {
                    // A static field: `우리의 'x'` or the quoted `'우리'의 'x'`.
                    let is_static = match &**object {
                        Expr::StaticRef => true,
                        Expr::Identifier(id) => self.lang.plural_self_words.contains(&id.as_str()),
                        _ => false,
                    };
                    if let (true, Expr::Identifier(prop)) = (is_static, &**property) {
                        return Some(decl(prop, false, true, value));
                    }
                }
                return Some(Stmt::Assign { target, value });
            }
            Kind::KwExecute => return Some(Stmt::Expr(first?)),
            Kind::KwReturn => return Some(Stmt::Return(first)),
            Kind::KwPrint => {
                let newline = !verb.lit.ends_with(self.lang.print_inline_suffix);
                return Some(Stmt::Print { value: first?, newline });
            }
            Kind::KwLoop => {
                let mut loop_var = String::new();
                if self.at(Kind::LParen) {
                    self.consume();
                    if self.at(Kind::Var) {
                        loop_var = strip(self.consume().lit, self.lang.delim_len);
                    }
                    if self.at(Kind::RParen) {
                        self.consume();
                    }
                }
                if !components.is_empty() {
                    match (self.lang.classify_loop)(verb.lit, &components) {
                        LoopKind::ForEach => {
                            let list = components.swap_remove(0).expr;
                            return Some(Stmt::ForEach { list, body: self.parse_block() });
                        }
                        LoopKind::While => {
                            let condition = components.swap_remove(0).expr;
                            return Some(Stmt::While { condition, body: self.parse_block() });
                        }
                        LoopKind::Range if components.len() >= 2 => {
                            let start = components[0].expr.clone();
                            let end = components[1].expr.clone();
                            return Some(Stmt::ForRange { start, end, loop_var, body: self.parse_block() });
                        }
                        LoopKind::Range => {}
                    }
                }
            }
            Kind::KwPush if components.len() >= 2 => {
                return Some(Stmt::ListPush {
                    target: components[0].expr.clone(),
                    value: components[1].expr.clone(),
                    position: self.list_position(&components[0].particles),
                });
            }
            Kind::KwPop if !components.is_empty() => {
                return Some(Stmt::ListPop {
                    target: components[0].expr.clone(),
                    position: self.list_position(&components[0].particles),
                });
            }
            Kind::KwInput => {
                let target = match first? {
                    Expr::Identifier(n) => Some(n),
                    _ => None,
                };
                let type_ref = match components.get(1).map(|c| &c.expr) {
                    Some(Expr::TypeRef(t)) => Some(t.clone()),
                    _ => None,
                };
                return Some(Stmt::Input { target, type_ref });
            }
            Kind::KwFallthrough => return Some(Stmt::Fallthrough),
            Kind::KwThrow if !components.is_empty() => return Some(Stmt::Throw(components[0].expr.clone())),
            Kind::KwMustHave if !components.is_empty() => return Some(Stmt::Expr(components[0].expr.clone())),
            Kind::KwSwitch if !components.is_empty() => {
                return Some(self.parse_switch_body(components[0].expr.clone()));
            }
            Kind::KwImport if components.len() >= 2 => return Some(self.import(&components)),
            _ => {}
        }
        None
    }

    fn parse_switch_body(&mut self, discriminant: Expr) -> Stmt {
        let mut cases = Vec::new();
        if self.at(Kind::Colon) {
            self.consume();
        }
        if self.at(Kind::Indent) {
            self.consume();
        }
        while let Some(k) = self.kind(0) {
            if k == Kind::Dedent || k == Kind::Eof {
                break;
            }
            if k == Kind::Indent {
                self.consume();
                continue;
            }
            if k == Kind::KwDefault {
                self.consume(); // 나머지는
                if self.at(Kind::Colon) {
                    self.consume();
                }
                let consequent = self.parse_block();
                cases.push(SwitchCase { tests: Vec::new(), consequent, is_default: true });
            } else {
                // "VIP", "일반" 인 경우:  (Kanade: 「VIP」の場合:)
                let mut tests = Vec::new();
                while let Some(k) = self.kind(0) {
                    if k == Kind::TypeIn {
                        self.consume(); // 인
                        if self.at(Kind::KwCase) {
                            self.consume();
                        }
                        break;
                    }
                    if k == Kind::KwCase {
                        self.consume();
                        break;
                    }
                    tests.push(self.parse_expression());
                    if self.at(Kind::Comma) {
                        self.consume();
                    }
                }
                if self.at(Kind::Colon) {
                    self.consume();
                }
                let consequent = self.parse_block();
                cases.push(SwitchCase { tests, consequent, is_default: false });
            }
        }
        if self.at(Kind::Dedent) {
            self.consume();
        }
        Stmt::Switch { discriminant, cases }
    }

    fn import(&self, components: &[Component]) -> Stmt {
        let (module, is_builtin) = match &components[0].expr {
            Expr::Identifier(id) => (id.clone(), false),
            Expr::Str(s) => (s.clone(), false),
            Expr::TypeRef(t) => (t.name.clone(), true),
            Expr::List(items) => match items.first() {
                Some(Expr::Identifier(id)) => (id.clone(), true),
                _ => (String::new(), false),
            },
            _ => (String::new(), false),
        };
        let rest = &components[1..];

        // [모듈]에서 전부 가져오자
        if rest.len() == 1 && rest[0].particles.is_empty() {
            if let Expr::Identifier(id) = &rest[0].expr {
                if id == self.lang.import_all_word {
                    return Stmt::Import { module, is_builtin, all: true, items: Vec::new() };
                }
            }
        }
        // <이름>을 <별칭>으로 가져오자
        if rest.len() == 2 && rest[1].particles.iter().any(|p| self.lang.import_as_particles.contains(&p.as_str())) {
            let item = ImportItem { name: import_name(&rest[0].expr), alias: import_name(&rest[1].expr) };
            return Stmt::Import { module, is_builtin, all: false, items: vec![item] };
        }
        let items = rest.iter().map(|c| ImportItem { name: import_name(&c.expr), alias: String::new() }).collect();
        Stmt::Import { module, is_builtin, all: false, items }
    }

    pub fn parse_expression(&mut self) -> Expr {
        self.parse_binary(1)
    }

    /// `* / %` bind tighter than `+ -`; left to right within a level.
    fn parse_binary(&mut self, min_prec: u8) -> Expr {
        let mut expr = self.parse_member_and_call();
        while let Some(t) = self.peek(0) {
            if t.kind != Kind::Op {
                break;
            }
            let prec = if matches!(t.lit, "*" | "/" | "%") { 2 } else { 1 };
            if prec < min_prec {
                break;
            }
            let op = self.consume().lit.to_string();
            let right = self.parse_binary(prec + 1);
            expr = Expr::Binary { left: Box::new(expr), op, right: Box::new(right) };
        }
        expr
    }

    fn parse_member_and_call(&mut self) -> Expr {
        let mut expr = self.parse_primary();
        while let Some(t) = self.peek(0) {
            let member = t.kind == Kind::Particle && t.lit == self.lang.member_particle;
            if member && self.kind(1) == Some(Kind::Ident) && self.lit(1) == Some(self.lang.popped_value_word) {
                // Kanade's decorative "の値" after an index: skip it.
                self.consume();
                self.consume();
            } else if member && !matches!(self.kind(1), Some(Kind::KwFront | Kind::KwBack)) {
                self.consume();
                let property = self.parse_primary();
                expr = Expr::Member { object: Box::new(expr), property: Box::new(property) };
            } else if member {
                // Kanade puts の before 前/後 too; leave the marker to the caller.
                self.consume();
            } else if t.kind == Kind::LParen {
                self.consume();
                let args = self.parse_args(Kind::RParen);
                expr = Expr::Call { callee: Box::new(expr), args };
            } else if matches!(t.kind, Kind::KwFront | Kind::KwBack) && self.kind(1) == Some(Kind::KwPopped) {
                // '목록' 뒤에서 꺼낸 (값)
                let position = if t.kind == Kind::KwFront { "front" } else { "back" };
                self.consume();
                self.consume();
                if self.at(Kind::Ident) && self.lit(0) == Some(self.lang.popped_value_word) {
                    self.consume();
                }
                expr = Expr::ListPop { target: Box::new(expr), position };
            } else {
                break;
            }
        }
        expr
    }

    /// Expressions up to `close` (consumed), separated by commas or particles.
    fn parse_args(&mut self, close: Kind) -> Vec<Expr> {
        let mut args = Vec::new();
        while let Some(k) = self.kind(0) {
            if k == close {
                break;
            }
            args.push(self.parse_expression());
            if matches!(self.kind(0), Some(Kind::Comma | Kind::Particle)) {
                self.consume();
            }
        }
        if self.at(close) {
            self.consume();
        }
        args
    }

    fn parse_primary(&mut self) -> Expr {
        let tok = self.consume();
        let d = self.lang.delim_len;
        match tok.kind {
            Kind::Op if tok.lit == "-" => {
                let next = self.consume();
                if next.kind == Kind::Int {
                    return Expr::Number(format!("-{}", next.lit).parse().unwrap_or(0.0));
                }
                Expr::Str("알수없음: -".to_string())
            }
            Kind::Str => Expr::Str(strip(tok.lit, d)),
            Kind::TemplateString => {
                let mut v = tok.lit;
                v = v.strip_prefix(self.lang.template_prefix).unwrap_or(v);
                v = v.strip_suffix(self.lang.template_suffix).unwrap_or(v);
                Expr::Template(v.to_string())
            }
            Kind::Var => Expr::Identifier(strip(tok.lit, d)),
            Kind::Function => {
                let name = strip(tok.lit, d);
                if name == self.lang.constructor_function_name {
                    Expr::FunctionRef("__init__".to_string())
                } else {
                    Expr::FunctionRef(name)
                }
            }
            Kind::KwNull => Expr::Null,
            Kind::KwTrue => Expr::Bool(true),
            Kind::KwFalse => Expr::Bool(false),
            Kind::Int => Expr::Number(tok.lit.parse().unwrap_or(0.0)),
            Kind::KwSelf => Expr::SelfRef,
            Kind::KwParent => Expr::SuperRef,
            Kind::Ident => {
                if self.lang.plural_self_words.contains(&tok.lit) {
                    Expr::StaticRef
                } else {
                    Expr::Identifier(tok.lit.to_string())
                }
            }
            Kind::KwNew => {
                let class = self.parse_type_ref();
                let mut args = Vec::new();
                if self.at(Kind::LParen) {
                    self.consume();
                    args = self.parse_args(Kind::RParen);
                }
                Expr::New { class, args }
            }
            Kind::Type => {
                let type_ref = TypeRef { name: self.type_name(tok.lit) };
                if self.at(Kind::TypeIn) || self.lit(0) == Some(self.lang.type_in_word) {
                    // `TYPE의 TYPE의 〈메서드〉()`: drop the outer, repeated type.
                    if self.kind(1) == Some(Kind::Type) {
                        self.consume();
                        return self.parse_primary();
                    }
                    // `TYPE의 〈정적메서드〉()` — only where the type word is the
                    // member particle (Kanade's の); Hari's 인 is always a type mark.
                    if self.kind(1) == Some(Kind::Function) && self.lit(0) == Some(self.lang.member_particle) {
                        self.consume();
                        let property = self.parse_primary();
                        return Expr::Member { object: Box::new(Expr::TypeRef(type_ref)), property: Box::new(property) };
                    }
                    self.consume(); // 인
                    let value = self.parse_primary();
                    self.declared_type = Some(type_ref);
                    return value;
                }
                Expr::TypeRef(type_ref)
            }
            Kind::LBracket => Expr::List(self.parse_args(Kind::RBracket)),
            Kind::LBrace => {
                let mut props = Vec::new();
                while let Some(k) = self.kind(0) {
                    if k == Kind::RBrace {
                        break;
                    }
                    let key = self.parse_expression();
                    if self.at(Kind::Colon) {
                        self.consume();
                    }
                    let value = self.parse_expression();
                    props.push((key, value));
                    if matches!(self.kind(0), Some(Kind::Comma | Kind::Particle)) {
                        self.consume();
                    }
                }
                if self.at(Kind::RBrace) {
                    self.consume();
                }
                Expr::Dict(props)
            }
            Kind::LParen => {
                let e = self.parse_condition();
                if self.at(Kind::RParen) {
                    self.consume();
                }
                e
            }
            _ => {
                let line = if tok.lit.is_empty() { self.last_content_line() } else { tok.line };
                self.diags.push(Diagnostic {
                    line,
                    col: tok.col,
                    length: tok.lit.chars().count() as u32,
                    literal: tok.lit.to_string(),
                });
                Expr::Str(format!("알수없음: {}", tok.lit))
            }
        }
    }

    fn parse_constructor(&mut self) -> Stmt {
        self.consume();
        let mut params = Vec::new();
        if self.at(Kind::LParen) {
            self.consume();
            params = self.parse_params(false);
            self.consume(); // )
        }
        if self.at(Kind::KwDoAs) {
            self.consume();
        }
        self.skip_until_colon();
        let body = self.parse_block();
        Stmt::Constructor { id: self.lang.constructor_function_name.to_string(), params, body: body.statements }
    }
}
