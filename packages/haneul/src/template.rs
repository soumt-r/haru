//! Templates in the manner of Jinja: `{{ 식 }}`, `{% if %}`, `{% for %}`,
//! `{% set %}`, `{% block %}`/`{% extends %}`, `{% include %}`, `{# 설명 #}`,
//! `{% raw %}`, filters (`|upper`) and tests (`is defined`).
//!
//! Differences from Jinja, to fit the language around it: indexes count
//! from 1 (`목록[1]` is the first item; -1 is the last), `참`/`거짓`/`비어있음`
//! (and `真`/`偽`/`空っぽ`) are literals besides `true`/`false`/`none`, and a
//! value prints as the program prints it (`비어있음` prints nothing). Output
//! is always HTML-escaped unless marked `|safe`. Block tags take the newline
//! after them and the indentation before them (Jinja's `trim_blocks` and
//! `lstrip_blocks`), and `{%-`/`-%}` trim all whitespace on that side.

use std::collections::HashMap;
use std::rc::Rc;

use crate::data::{key_cmp, number, same, to_json, Lang, V};
use crate::util::{html_escape, percent_encode};

pub struct Template {
    name: String,
    nodes: Vec<Node>,
    extends: Option<Expr>,
    blocks: HashMap<String, Rc<Vec<Node>>>,
}

enum Node {
    Text(String),
    Out(Expr, usize),
    If(Vec<(Expr, Vec<Node>)>, Vec<Node>),
    For { vars: Vec<String>, iter: Expr, body: Vec<Node>, empty: Vec<Node>, line: usize },
    Set(Vec<String>, Expr, usize),
    Block(String),
    Include(Expr, usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
    FloorDiv,
    Mod,
    Concat,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    In,
    NotIn,
    And,
    Or,
}

enum Expr {
    Lit(V),
    Var(String),
    List(Vec<Expr>),
    Dict(Vec<(Expr, Expr)>),
    Attr(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    Call(Box<Expr>, Vec<Expr>),
    Filter(Box<Expr>, String, Vec<Expr>),
    Test(Box<Expr>, String, Vec<Expr>, bool),
    Not(Box<Expr>),
    Neg(Box<Expr>),
    Bin(Op, Box<Expr>, Box<Expr>),
    Cond(Box<Expr>, Box<Expr>, Option<Box<Expr>>),
}

/// A failure, already in the program's language.
pub type Fail = String;

// ---------------------------------------------------------------------------
// Reading

enum Tok {
    Text(String),
    Out(String, usize),
    Stmt(String, usize),
}

fn at(lang: Lang, name: &str, line: usize, what: String) -> Fail {
    match lang {
        Lang::Hari => format!("{name} {line}번째 줄: {what}"),
        Lang::Kanade => format!("{name} {line}行目: {what}"),
    }
}

/// Splits the text into literal text and tags.
fn lex(src: &str, name: &str, lang: Lang) -> Result<Vec<Tok>, Fail> {
    let mut toks = Vec::new();
    let mut pos = 0;
    let mut line = 1;
    // The last tag asked to trim what follows it (`-%}`), or takes one newline.
    let mut trim_next = false;
    let mut newline_next = false;
    while pos <= src.len() {
        let rest = &src[pos..];
        let start = ["{{", "{%", "{#"].iter().filter_map(|o| rest.find(o)).min();
        let text_end = start.map_or(src.len(), |s| pos + s);
        let mut text = src[pos..text_end].to_string();
        // Whether the text starts a line (for lstrip_blocks).
        let mut begins_line = pos == 0 || src[..pos].ends_with('\n');
        if trim_next {
            text = text.trim_start().to_string();
        } else if newline_next {
            if let Some(t) = text.strip_prefix("\r\n").or_else(|| text.strip_prefix('\n')) {
                text = t.to_string();
                begins_line = true;
            }
        }
        let Some(_) = start else {
            if !text.is_empty() {
                toks.push(Tok::Text(text));
            }
            break;
        };
        line += src[pos..text_end].matches('\n').count();
        let open = &src[text_end..text_end + 2];
        let close = match open {
            "{{" => "}}",
            "{%" => "%}",
            _ => "#}",
        };
        let inner_start = text_end + 2;
        let minus_open = src[inner_start..].starts_with('-');
        let Some(len) = find_close(&src[inner_start..], close, open != "{#") else {
            return Err(at(lang, name, line, lang.tr(&format!("'{open}'가 닫히지 않았어요."), &format!("「{open}」が閉じられていません。"))));
        };
        let inner_end = inner_start + len;
        let mut inner = &src[inner_start..inner_end];
        let minus_close = inner.ends_with('-') && inner.len() > usize::from(minus_open);
        if minus_open {
            inner = &inner[1..];
        }
        if minus_close {
            inner = &inner[..inner.len() - 1];
        }
        let block = open != "{{";
        if minus_open {
            text = text.trim_end().to_string();
        } else if block {
            // lstrip_blocks: the tag's indentation on its own line goes.
            let (line_start, at_line_start) = match text.rfind('\n') {
                Some(i) => (i + 1, true),
                None => (0, begins_line),
            };
            if at_line_start && text[line_start..].chars().all(|c| c == ' ' || c == '\t') {
                text.truncate(line_start);
            }
        }
        if !text.is_empty() {
            toks.push(Tok::Text(text));
        }
        let tag_line = line;
        line += inner.matches('\n').count();
        pos = inner_end + 2;
        trim_next = minus_close;
        newline_next = block;
        match open {
            "{{" => toks.push(Tok::Out(inner.trim().to_string(), tag_line)),
            "{%" => {
                let stmt = inner.trim();
                if stmt == "raw" {
                    let Some((raw, end, lines)) = find_endraw(&src[pos..]) else {
                        return Err(at(lang, name, tag_line, lang.tr("{% raw %}가 닫히지 않았어요.", "{% raw %}が閉じられていません。")));
                    };
                    let mut raw = raw.to_string();
                    if let Some(t) = raw.strip_prefix("\r\n").or_else(|| raw.strip_prefix('\n')) {
                        raw = t.to_string();
                    }
                    toks.push(Tok::Text(raw));
                    line += lines;
                    pos += end;
                } else {
                    toks.push(Tok::Stmt(stmt.to_string(), tag_line));
                }
            }
            _ => {}
        }
    }
    Ok(toks)
}

/// Where `close` ends the tag (skipping quoted text when `quotes`).
fn find_close(s: &str, close: &str, quotes: bool) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < b.len() {
        match quote {
            Some(_) if b[i] == b'\\' => i += 1,
            Some(q) if b[i] == q => quote = None,
            Some(_) => {}
            None if quotes && (b[i] == b'"' || b[i] == b'\'') => quote = Some(b[i]),
            None if b[i..].starts_with(close.as_bytes()) => return Some(i),
            None => {}
        }
        i += 1;
    }
    None
}

/// The raw text up to `{% endraw %}`, where the text after that tag starts,
/// and the lines both take.
fn find_endraw(s: &str) -> Option<(&str, usize, usize)> {
    let mut from = 0;
    while let Some(i) = s[from..].find("{%") {
        let start = from + i;
        let inner = &s[start + 2..];
        if let Some(len) = inner.find("%}") {
            let word = inner[..len].trim_matches(|c: char| c == '-' || c.is_whitespace());
            if word == "endraw" {
                let end = start + 2 + len + 2;
                return Some((&s[..start], end, s[..end].matches('\n').count()));
            }
        }
        from = start + 2;
    }
    None
}

/// Reads a template.
pub fn parse(src: &str, name: &str, lang: Lang) -> Result<Template, Fail> {
    let toks = lex(src, name, lang)?;
    let mut p = Parser { toks, i: 0, name, lang, blocks: HashMap::new(), extends: None };
    let (nodes, end) = p.nodes(&[])?;
    if let Some((word, line)) = end {
        return Err(at(lang, name, line, lang.tr(&format!("여기에 {{% {word} %}}가 올 수 없어요."), &format!("ここに{{% {word} %}}は来られません。"))));
    }
    Ok(Template { name: name.to_string(), nodes, extends: p.extends, blocks: p.blocks })
}

struct Parser<'a> {
    toks: Vec<Tok>,
    i: usize,
    name: &'a str,
    lang: Lang,
    blocks: HashMap<String, Rc<Vec<Node>>>,
    extends: Option<Expr>,
}

impl Parser<'_> {
    fn err(&self, line: usize, ko: &str, ja: &str) -> Fail {
        at(self.lang, self.name, line, self.lang.tr(ko, ja))
    }

    fn expr(&self, text: &str, line: usize) -> Result<Expr, Fail> {
        let mut e = ExprParser::new(text, self.lang).map_err(|m| at(self.lang, self.name, line, m))?;
        let x = e.expr().map_err(|m| at(self.lang, self.name, line, m))?;
        e.end().map_err(|m| at(self.lang, self.name, line, m))?;
        Ok(x)
    }

    /// Nodes up to one of the `ends` words; that word (with the rest of its
    /// tag) and its line, or `None` at the end of the template.
    #[allow(clippy::type_complexity)]
    fn nodes(&mut self, ends: &[&str]) -> Result<(Vec<Node>, Option<(String, usize)>), Fail> {
        let mut nodes = Vec::new();
        while self.i < self.toks.len() {
            let tok = std::mem::replace(&mut self.toks[self.i], Tok::Text(String::new()));
            self.i += 1;
            match tok {
                Tok::Text(t) => nodes.push(Node::Text(t)),
                Tok::Out(e, line) => {
                    if e.is_empty() {
                        return Err(self.err(line, "{{ }} 안이 비어 있어요.", "{{ }}の中が空です。"));
                    }
                    nodes.push(Node::Out(self.expr(&e, line)?, line));
                }
                Tok::Stmt(s, line) => {
                    let (word, rest) = split_word(&s);
                    if ends.contains(&word) {
                        return Ok((nodes, Some((s.clone(), line))));
                    }
                    nodes.push(match word {
                        "if" => self.if_node(rest, line)?,
                        "for" => self.for_node(rest, line)?,
                        "set" => {
                            let Some((names, value)) = rest.split_once('=') else {
                                return Err(self.err(line, "{% set 이름 = 값 %}처럼 써요.", "{% set 名前 = 値 %}のように書きます。"));
                            };
                            Node::Set(self.names(names, line)?, self.expr(value, line)?, line)
                        }
                        "block" => {
                            let name = rest.trim().to_string();
                            if !is_name(&name) {
                                return Err(self.err(line, "블록 이름이 올바르지 않아요.", "ブロック名が正しくありません。"));
                            }
                            let (body, end) = self.nodes(&["endblock"])?;
                            match end {
                                Some((e, l)) if !matches!(split_word(&e).1.trim(), n if n.is_empty() || n == name) => {
                                    return Err(self.err(l, "블록 이름이 맞지 않아요.", "ブロック名が合いません。"));
                                }
                                Some(_) => {}
                                None => return Err(self.unclosed("block", line)),
                            }
                            if self.blocks.insert(name.clone(), Rc::new(body)).is_some() {
                                return Err(self.err(line, &format!("블록 '{name}'이 두 번 있어요."), &format!("ブロック「{name}」が二つあります。")));
                            }
                            Node::Block(name)
                        }
                        "extends" => {
                            if self.extends.is_some() {
                                return Err(self.err(line, "extends는 한 번만 쓸 수 있어요.", "extendsは一度しか書けません。"));
                            }
                            self.extends = Some(self.expr(rest, line)?);
                            continue;
                        }
                        "include" => Node::Include(self.expr(rest, line)?, line),
                        "" => return Err(self.err(line, "{% %} 안이 비어 있어요.", "{% %}の中が空です。")),
                        other => {
                            return Err(self.err(line, &format!("알 수 없는 태그예요: {other}"), &format!("知らないタグです: {other}")));
                        }
                    });
                }
            }
        }
        Ok((nodes, None))
    }

    fn unclosed(&self, word: &str, line: usize) -> Fail {
        self.err(line, &format!("{{% {word} %}}가 닫히지 않았어요 ({{% end{word} %}}가 없어요)."), &format!("{{% {word} %}}が閉じられていません（{{% end{word} %}}がありません）。"))
    }

    fn names(&self, text: &str, line: usize) -> Result<Vec<String>, Fail> {
        let names: Vec<String> = text.split(',').map(|n| n.trim().to_string()).collect();
        if names.iter().all(|n| is_name(n)) {
            Ok(names)
        } else {
            Err(self.err(line, &format!("변수 이름이 올바르지 않아요: {}", text.trim()), &format!("変数名が正しくありません: {}", text.trim())))
        }
    }

    fn if_node(&mut self, cond: &str, line: usize) -> Result<Node, Fail> {
        let mut arms = vec![];
        let mut cond = self.expr(cond, line)?;
        loop {
            let (body, end) = self.nodes(&["elif", "else", "endif"])?;
            arms.push((cond, body));
            let Some((e, l)) = end else { return Err(self.unclosed("if", line)) };
            match split_word(&e) {
                ("elif", rest) => cond = self.expr(rest, l)?,
                ("else", _) => {
                    let (other, end) = self.nodes(&["endif"])?;
                    if end.is_none() {
                        return Err(self.unclosed("if", line));
                    }
                    return Ok(Node::If(arms, other));
                }
                _ => return Ok(Node::If(arms, Vec::new())),
            }
        }
    }

    fn for_node(&mut self, head: &str, line: usize) -> Result<Node, Fail> {
        let Some((names, iter)) = split_in(head) else {
            return Err(self.err(line, "{% for 이름 in 목록 %}처럼 써요.", "{% for 名前 in リスト %}のように書きます。"));
        };
        let vars = self.names(names, line)?;
        let iter = self.expr(iter, line)?;
        let (body, end) = self.nodes(&["else", "endfor"])?;
        let Some((e, _)) = end else { return Err(self.unclosed("for", line)) };
        let empty = if split_word(&e).0 == "else" {
            let (empty, end) = self.nodes(&["endfor"])?;
            if end.is_none() {
                return Err(self.unclosed("for", line));
            }
            empty
        } else {
            Vec::new()
        };
        Ok(Node::For { vars, iter, body, empty, line })
    }
}

fn split_word(s: &str) -> (&str, &str) {
    let s = s.trim();
    match s.find(char::is_whitespace) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    }
}

/// `a, b in 목록` at the first ` in ` outside of quotes.
fn split_in(s: &str) -> Option<(&str, &str)> {
    let i = s.find(" in ")?;
    Some((&s[..i], &s[i + 4..]))
}

fn is_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_alphabetic() || c == '_') && chars.all(|c| c.is_alphanumeric() || c == '_')
}

// ---------------------------------------------------------------------------
// Expressions

#[derive(Clone, Debug, PartialEq)]
enum T {
    Num(f64),
    Str(String),
    Name(String),
    P(&'static str),
    End,
}

struct ExprParser {
    toks: Vec<T>,
    i: usize,
    lang: Lang,
}

const PUNCT: [&str; 23] =
    ["==", "!=", "<=", ">=", "//", "<", ">", "+", "-", "*", "/", "%", "~", "|", ".", "[", "]", "(", ")", "{", "}", ",", ":"];

impl ExprParser {
    fn new(text: &str, lang: Lang) -> Result<ExprParser, Fail> {
        let mut toks = Vec::new();
        let cs: Vec<char> = text.chars().collect();
        let mut i = 0;
        while i < cs.len() {
            let c = cs[i];
            if c.is_whitespace() {
                i += 1;
            } else if c.is_ascii_digit() {
                let s = i;
                while i < cs.len() && (cs[i].is_ascii_digit() || cs[i] == '_') {
                    i += 1;
                }
                if i + 1 < cs.len() && cs[i] == '.' && cs[i + 1].is_ascii_digit() {
                    i += 1;
                    while i < cs.len() && cs[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                let t: String = cs[s..i].iter().filter(|c| **c != '_').collect();
                toks.push(T::Num(t.parse().unwrap_or(0.0)));
            } else if c == '"' || c == '\'' {
                let mut s = String::new();
                i += 1;
                loop {
                    let Some(&d) = cs.get(i) else {
                        return Err(lang.tr("글이 닫히지 않았어요.", "文字列が閉じられていません。"));
                    };
                    i += 1;
                    if d == c {
                        break;
                    }
                    if d == '\\' {
                        let e = cs.get(i).copied().unwrap_or('\\');
                        i += 1;
                        s.push(match e {
                            'n' => '\n',
                            't' => '\t',
                            'r' => '\r',
                            other => other,
                        });
                    } else {
                        s.push(d);
                    }
                }
                toks.push(T::Str(s));
            } else if c.is_alphabetic() || c == '_' {
                let s = i;
                while i < cs.len() && (cs[i].is_alphanumeric() || cs[i] == '_') {
                    i += 1;
                }
                toks.push(T::Name(cs[s..i].iter().collect()));
            } else {
                let rest: String = cs[i..cs.len().min(i + 2)].iter().collect();
                let Some(p) = PUNCT.iter().find(|p| rest.starts_with(**p)) else {
                    return Err(lang.tr(&format!("이해할 수 없는 글자예요: {c}"), &format!("理解できない文字です: {c}")));
                };
                toks.push(T::P(p));
                i += p.chars().count();
            }
        }
        toks.push(T::End);
        Ok(ExprParser { toks, i: 0, lang })
    }

    fn peek(&self) -> &T {
        &self.toks[self.i]
    }

    fn peek_at(&self, n: usize) -> &T {
        self.toks.get(self.i + n).unwrap_or(&T::End)
    }

    fn next(&mut self) -> T {
        let t = self.toks[self.i].clone();
        if t != T::End {
            self.i += 1;
        }
        t
    }

    fn is_p(&self, p: &str) -> bool {
        matches!(self.peek(), T::P(x) if *x == p)
    }

    fn is_word(&self, w: &str) -> bool {
        matches!(self.peek(), T::Name(x) if x == w)
    }

    fn eat_p(&mut self, p: &str) -> bool {
        if self.is_p(p) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn eat_word(&mut self, w: &str) -> bool {
        if self.is_word(w) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn unexpected(&self) -> Fail {
        let what = match self.peek() {
            T::End => return self.lang.tr("식이 덜 끝났어요.", "式が途中で終わっています。"),
            T::Num(n) => number(*n),
            T::Str(s) => format!("\"{s}\""),
            T::Name(n) => n.clone(),
            T::P(p) => p.to_string(),
        };
        self.lang.tr(&format!("여기에 '{what}'가 올 수 없어요."), &format!("ここに「{what}」は来られません。"))
    }

    fn want_p(&mut self, p: &str) -> Result<(), Fail> {
        if self.eat_p(p) {
            Ok(())
        } else {
            Err(self.unexpected())
        }
    }

    fn end(&self) -> Result<(), Fail> {
        if *self.peek() == T::End {
            Ok(())
        } else {
            Err(self.unexpected())
        }
    }

    fn expr(&mut self) -> Result<Expr, Fail> {
        let then = self.or()?;
        if self.eat_word("if") {
            let cond = self.or()?;
            let other = if self.eat_word("else") { Some(Box::new(self.expr()?)) } else { None };
            return Ok(Expr::Cond(Box::new(then), Box::new(cond), other));
        }
        Ok(then)
    }

    fn or(&mut self) -> Result<Expr, Fail> {
        let mut e = self.and()?;
        while self.eat_word("or") {
            e = Expr::Bin(Op::Or, Box::new(e), Box::new(self.and()?));
        }
        Ok(e)
    }

    fn and(&mut self) -> Result<Expr, Fail> {
        let mut e = self.not()?;
        while self.eat_word("and") {
            e = Expr::Bin(Op::And, Box::new(e), Box::new(self.not()?));
        }
        Ok(e)
    }

    fn not(&mut self) -> Result<Expr, Fail> {
        if self.eat_word("not") {
            return Ok(Expr::Not(Box::new(self.not()?)));
        }
        self.compare()
    }

    fn compare(&mut self) -> Result<Expr, Fail> {
        let mut e = self.concat()?;
        loop {
            let op = match self.peek() {
                T::P("==") => Op::Eq,
                T::P("!=") => Op::Ne,
                T::P("<") => Op::Lt,
                T::P(">") => Op::Gt,
                T::P("<=") => Op::Le,
                T::P(">=") => Op::Ge,
                T::Name(n) if n == "in" => Op::In,
                T::Name(n) if n == "not" && matches!(self.peek_at(1), T::Name(m) if m == "in") => {
                    self.i += 1;
                    Op::NotIn
                }
                T::Name(n) if n == "is" => {
                    self.i += 1;
                    let negated = self.eat_word("not");
                    let T::Name(test) = self.next() else { return Err(self.unexpected()) };
                    let args = if self.is_p("(") {
                        self.i += 1;
                        self.args(")")?
                    } else if matches!(self.peek(), T::Num(_) | T::Str(_)) {
                        vec![self.primary()?]
                    } else {
                        Vec::new()
                    };
                    e = Expr::Test(Box::new(e), test, args, negated);
                    continue;
                }
                _ => return Ok(e),
            };
            self.i += 1;
            e = Expr::Bin(op, Box::new(e), Box::new(self.concat()?));
        }
    }

    fn concat(&mut self) -> Result<Expr, Fail> {
        let mut e = self.add()?;
        while self.eat_p("~") {
            e = Expr::Bin(Op::Concat, Box::new(e), Box::new(self.add()?));
        }
        Ok(e)
    }

    fn add(&mut self) -> Result<Expr, Fail> {
        let mut e = self.mul()?;
        loop {
            let op = if self.eat_p("+") {
                Op::Add
            } else if self.eat_p("-") {
                Op::Sub
            } else {
                return Ok(e);
            };
            e = Expr::Bin(op, Box::new(e), Box::new(self.mul()?));
        }
    }

    fn mul(&mut self) -> Result<Expr, Fail> {
        let mut e = self.unary()?;
        loop {
            let op = if self.eat_p("*") {
                Op::Mul
            } else if self.eat_p("//") {
                Op::FloorDiv
            } else if self.eat_p("/") {
                Op::Div
            } else if self.eat_p("%") {
                Op::Mod
            } else {
                return Ok(e);
            };
            e = Expr::Bin(op, Box::new(e), Box::new(self.unary()?));
        }
    }

    fn unary(&mut self) -> Result<Expr, Fail> {
        if self.eat_p("-") {
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        if self.eat_p("+") {
            return self.unary();
        }
        self.postfix()
    }

    fn args(&mut self, close: &str) -> Result<Vec<Expr>, Fail> {
        let mut args = Vec::new();
        if self.eat_p(close) {
            return Ok(args);
        }
        loop {
            args.push(self.expr()?);
            if self.eat_p(close) {
                return Ok(args);
            }
            self.want_p(",")?;
            // A comma may end the list.
            if self.eat_p(close) {
                return Ok(args);
            }
        }
    }

    fn postfix(&mut self) -> Result<Expr, Fail> {
        let mut e = self.primary()?;
        loop {
            if self.eat_p(".") {
                match self.next() {
                    T::Name(n) => e = Expr::Attr(Box::new(e), n),
                    // `목록.1`, as Jinja allows.
                    T::Num(n) => e = Expr::Index(Box::new(e), Box::new(Expr::Lit(V::Num(n)))),
                    _ => {
                        self.i -= 1;
                        return Err(self.unexpected());
                    }
                }
            } else if self.eat_p("[") {
                let index = self.expr()?;
                self.want_p("]")?;
                e = Expr::Index(Box::new(e), Box::new(index));
            } else if self.eat_p("(") {
                let args = self.args(")")?;
                e = Expr::Call(Box::new(e), args);
            } else if self.eat_p("|") {
                let T::Name(name) = self.next() else {
                    self.i -= 1;
                    return Err(self.unexpected());
                };
                let args = if self.eat_p("(") { self.args(")")? } else { Vec::new() };
                e = Expr::Filter(Box::new(e), name, args);
            } else {
                return Ok(e);
            }
        }
    }

    fn primary(&mut self) -> Result<Expr, Fail> {
        match self.next() {
            T::Num(n) => Ok(Expr::Lit(V::Num(n))),
            T::Str(s) => Ok(Expr::Lit(V::str(&s))),
            T::Name(n) => Ok(match n.as_str() {
                "true" | "True" | "참" | "真" => Expr::Lit(V::Bool(true)),
                "false" | "False" | "거짓" | "偽" => Expr::Lit(V::Bool(false)),
                "none" | "None" | "비어있음" | "空っぽ" => Expr::Lit(V::Null),
                "and" | "or" | "not" | "in" | "is" | "if" | "else" => {
                    self.i -= 1;
                    return Err(self.unexpected());
                }
                _ => Expr::Var(n),
            }),
            T::P("(") => {
                let e = self.expr()?;
                self.want_p(")")?;
                Ok(e)
            }
            T::P("[") => Ok(Expr::List(self.args("]")?)),
            T::P("{") => {
                let mut entries = Vec::new();
                if !self.eat_p("}") {
                    loop {
                        let k = self.expr()?;
                        self.want_p(":")?;
                        entries.push((k, self.expr()?));
                        if self.eat_p("}") {
                            break;
                        }
                        self.want_p(",")?;
                        if self.eat_p("}") {
                            break;
                        }
                    }
                }
                Ok(Expr::Dict(entries))
            }
            _ => {
                self.i = self.i.saturating_sub(1);
                Err(self.unexpected())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering

/// Finds a template by name (for `extends` and `include`).
pub type Loader<'a> = dyn FnMut(&str) -> Result<Rc<Template>, Fail> + 'a;

const MAX_DEPTH: usize = 64;

struct Renderer<'a, 'l> {
    lang: Lang,
    loader: &'a mut Loader<'l>,
    scopes: Vec<HashMap<String, V>>,
    /// Each block's definitions, the most derived first.
    blocks: HashMap<String, Vec<Rc<Vec<Node>>>>,
    /// The block being rendered and which of its definitions (for `super()`).
    supers: Vec<(String, usize)>,
    name: String,
    depth: usize,
}

/// Renders a template with variables (a dictionary's text keys).
pub fn render(t: &Rc<Template>, vars: &V, lang: Lang, loader: &mut Loader<'_>) -> Result<String, Fail> {
    render_at(t, vars, lang, loader, 0)
}

fn render_at(t: &Rc<Template>, vars: &V, lang: Lang, loader: &mut Loader<'_>, depth: usize) -> Result<String, Fail> {
    let mut scope = HashMap::new();
    if let V::Dict(entries) = vars {
        for (k, v) in entries.iter() {
            if let Some(k) = k.as_text() {
                scope.insert(k.to_string(), v.clone());
            }
        }
    }
    let mut r = Renderer { lang, loader, scopes: vec![scope], blocks: HashMap::new(), supers: Vec::new(), name: t.name.clone(), depth };
    // The chain of templates each extends, down to the one with the page.
    let mut chain = vec![t.clone()];
    loop {
        let last = chain.last().unwrap().clone();
        let Some(parent) = &last.extends else { break };
        if chain.len() > MAX_DEPTH {
            return Err(r.fail(0, "extends가 너무 깊어요.", "extendsが深すぎます。"));
        }
        let name = r.eval(parent, 0)?;
        let Some(name) = name.as_text() else {
            return Err(r.fail(0, "extends에는 템플릿 이름(글)을 써요.", "extendsにはテンプレート名（文字列）を書きます。"));
        };
        let base = (r.loader)(name)?;
        chain.push(base);
    }
    for tmpl in &chain {
        for (name, body) in &tmpl.blocks {
            r.blocks.entry(name.clone()).or_default().push(body.clone());
        }
    }
    let base = chain.last().unwrap().clone();
    r.name = base.name.clone();
    let mut out = String::new();
    r.nodes(&base.nodes, &mut out)?;
    Ok(out)
}

enum Flow {
    Normal,
}

impl Renderer<'_, '_> {
    fn fail(&self, line: usize, ko: &str, ja: &str) -> Fail {
        if line == 0 {
            format!("{}: {}", self.name, self.lang.tr(ko, ja))
        } else {
            at(self.lang, &self.name, line, self.lang.tr(ko, ja))
        }
    }

    fn lookup(&self, name: &str) -> V {
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.get(name) {
                return v.clone();
            }
        }
        V::Undef
    }

    fn nodes(&mut self, nodes: &[Node], out: &mut String) -> Result<Flow, Fail> {
        for node in nodes {
            match node {
                Node::Text(t) => out.push_str(t),
                Node::Out(e, line) => {
                    let v = self.eval(e, *line)?;
                    match &v {
                        V::Safe(s) => out.push_str(s),
                        other => out.push_str(&html_escape(&other.text(self.lang))),
                    }
                }
                Node::If(arms, other) => {
                    let mut done = false;
                    for (cond, body) in arms {
                        if self.eval(cond, 0)?.truthy() {
                            self.nodes(body, out)?;
                            done = true;
                            break;
                        }
                    }
                    if !done {
                        self.nodes(other, out)?;
                    }
                }
                Node::For { vars, iter, body, empty, line } => {
                    let seq = self.eval(iter, *line)?;
                    let items: Vec<V> = match &seq {
                        V::List(items) => items.iter().cloned().collect(),
                        V::Dict(entries) => entries.iter().map(|(k, _)| k.clone()).collect(),
                        V::Str(s) | V::Safe(s) => s.chars().map(|c| V::str(&c.to_string())).collect(),
                        V::Undef | V::Null => Vec::new(),
                        other => {
                            let t = other.text(self.lang);
                            return Err(self.fail(*line, &format!("반복할 수 없는 값이에요: {t}"), &format!("繰り返せない値です: {t}")));
                        }
                    };
                    if items.is_empty() {
                        self.nodes(empty, out)?;
                        continue;
                    }
                    let n = items.len();
                    for (i, item) in items.into_iter().enumerate() {
                        let mut scope = HashMap::new();
                        if vars.len() == 1 {
                            scope.insert(vars[0].clone(), item);
                        } else {
                            let parts: Vec<V> = match &item {
                                V::List(p) if p.len() == vars.len() => p.iter().cloned().collect(),
                                _ => {
                                    return Err(self.fail(*line, &format!("항목을 {}개로 나눌 수 없어요.", vars.len()), &format!("要素を{}個に分けられません。", vars.len())));
                                }
                            };
                            for (name, part) in vars.iter().zip(parts) {
                                scope.insert(name.clone(), part);
                            }
                        }
                        let num = |x: usize| V::Num(x as f64);
                        scope.insert(
                            "loop".into(),
                            V::dict(vec![
                                (V::str("index"), num(i + 1)),
                                (V::str("index0"), num(i)),
                                (V::str("revindex"), num(n - i)),
                                (V::str("revindex0"), num(n - i - 1)),
                                (V::str("first"), V::Bool(i == 0)),
                                (V::str("last"), V::Bool(i + 1 == n)),
                                (V::str("length"), num(n)),
                            ]),
                        );
                        self.scopes.push(scope);
                        let r = self.nodes(body, out);
                        self.scopes.pop();
                        r?;
                    }
                }
                Node::Set(names, e, line) => {
                    let v = self.eval(e, *line)?;
                    let scope = self.scopes.last_mut().unwrap();
                    if names.len() == 1 {
                        scope.insert(names[0].clone(), v);
                    } else {
                        match &v {
                            V::List(p) if p.len() == names.len() => {
                                for (name, part) in names.iter().zip(p.iter()) {
                                    scope.insert(name.clone(), part.clone());
                                }
                            }
                            _ => {
                                return Err(self.fail(*line, &format!("값을 {}개로 나눌 수 없어요.", names.len()), &format!("値を{}個に分けられません。", names.len())));
                            }
                        }
                    }
                }
                Node::Block(name) => {
                    if let Some(first) = self.blocks.get(name).and_then(|d| d.first()).cloned() {
                        self.supers.push((name.clone(), 0));
                        let r = self.nodes(&first, out);
                        self.supers.pop();
                        r?;
                    }
                }
                Node::Include(e, line) => {
                    let name = self.eval(e, *line)?;
                    let Some(name) = name.as_text() else {
                        return Err(self.fail(*line, "include에는 템플릿 이름(글)을 써요.", "includeにはテンプレート名（文字列）を書きます。"));
                    };
                    if self.depth >= MAX_DEPTH {
                        return Err(self.fail(*line, "include가 너무 깊어요.", "includeが深すぎます。"));
                    }
                    let t = (self.loader)(name)?;
                    let vars = self.flat_vars();
                    let text = render_at(&t, &vars, self.lang, self.loader, self.depth + 1)?;
                    out.push_str(&text);
                }
            }
        }
        Ok(Flow::Normal)
    }

    /// Every variable in sight, as one dictionary.
    fn flat_vars(&self) -> V {
        let mut all: HashMap<&str, &V> = HashMap::new();
        for scope in &self.scopes {
            for (k, v) in scope {
                all.insert(k, v);
            }
        }
        V::dict(all.into_iter().map(|(k, v)| (V::str(k), v.clone())).collect())
    }

    fn eval(&mut self, e: &Expr, line: usize) -> Result<V, Fail> {
        Ok(match e {
            Expr::Lit(v) => v.clone(),
            Expr::Var(n) => self.lookup(n),
            Expr::List(items) => V::List(Rc::new(items.iter().map(|x| self.eval(x, line)).collect::<Result<_, _>>()?)),
            Expr::Dict(entries) => {
                let mut out = Vec::new();
                for (k, v) in entries {
                    out.push((self.eval(k, line)?, self.eval(v, line)?));
                }
                V::dict(out)
            }
            Expr::Attr(x, name) => {
                let v = self.eval(x, line)?;
                v.get_str(name).cloned().unwrap_or(V::Undef)
            }
            Expr::Index(x, i) => {
                let v = self.eval(x, line)?;
                let i = self.eval(i, line)?;
                self.index(&v, &i, line)?
            }
            Expr::Call(f, args) => {
                let args = args.iter().map(|a| self.eval(a, line)).collect::<Result<Vec<_>, _>>()?;
                self.call(f, args, line)?
            }
            Expr::Filter(x, name, args) => {
                let v = self.eval(x, line)?;
                let args = args.iter().map(|a| self.eval(a, line)).collect::<Result<Vec<_>, _>>()?;
                self.filter(name, v, &args, line)?
            }
            Expr::Test(x, name, args, negated) => {
                let v = self.eval(x, line)?;
                let args = args.iter().map(|a| self.eval(a, line)).collect::<Result<Vec<_>, _>>()?;
                V::Bool(self.test(name, &v, &args, line)? != *negated)
            }
            Expr::Not(x) => V::Bool(!self.eval(x, line)?.truthy()),
            Expr::Neg(x) => match self.eval(x, line)? {
                V::Num(n) => V::Num(-n),
                other => return Err(self.not_number(&other, line)),
            },
            Expr::Bin(Op::And, a, b) => {
                let a = self.eval(a, line)?;
                if !a.truthy() {
                    a
                } else {
                    self.eval(b, line)?
                }
            }
            Expr::Bin(Op::Or, a, b) => {
                let a = self.eval(a, line)?;
                if a.truthy() {
                    a
                } else {
                    self.eval(b, line)?
                }
            }
            Expr::Bin(op, a, b) => {
                let a = self.eval(a, line)?;
                let b = self.eval(b, line)?;
                self.binary(*op, a, b, line)?
            }
            Expr::Cond(then, cond, other) => {
                if self.eval(cond, line)?.truthy() {
                    self.eval(then, line)?
                } else {
                    match other {
                        Some(o) => self.eval(o, line)?,
                        None => V::Undef,
                    }
                }
            }
        })
    }

    fn not_number(&self, v: &V, line: usize) -> Fail {
        let t = v.text(self.lang);
        self.fail(line, &format!("숫자가 아니에요: {t}"), &format!("数ではありません: {t}"))
    }

    fn index(&self, v: &V, i: &V, line: usize) -> Result<V, Fail> {
        Ok(match (v, i) {
            (V::List(items), V::Num(n)) => pick(items.len(), *n).map_or(V::Undef, |k| items[k].clone()),
            (V::Str(s) | V::Safe(s), V::Num(n)) => {
                let chars: Vec<char> = s.chars().collect();
                pick(chars.len(), *n).map_or(V::Undef, |k| V::str(&chars[k].to_string()))
            }
            (V::Dict(_), key) => v.get(key).cloned().unwrap_or(V::Undef),
            (V::Undef | V::Null, _) => V::Undef,
            (_, other) => {
                let t = other.text(self.lang);
                return Err(self.fail(line, &format!("이 값으로는 찾을 수 없어요: {t}"), &format!("この値では取り出せません: {t}")));
            }
        })
    }

    fn call(&mut self, f: &Expr, args: Vec<V>, line: usize) -> Result<V, Fail> {
        let num = |i: usize| match args.get(i) {
            Some(V::Num(n)) => Some(*n),
            _ => None,
        };
        match f {
            Expr::Var(n) if n == "range" => {
                let (start, end, step) = match args.len() {
                    1 => (1.0, num(0), 1.0),
                    2 => (num(0).unwrap_or(f64::NAN), num(1), 1.0),
                    3 => (num(0).unwrap_or(f64::NAN), num(1), num(2).unwrap_or(0.0)),
                    _ => (f64::NAN, None, 1.0),
                };
                let Some(end) = end.filter(|_| start.is_finite() && step != 0.0) else {
                    return Err(self.fail(line, "range(끝), range(시작, 끝), range(시작, 끝, 간격)처럼 써요.", "range(終わり)、range(始め, 終わり)、range(始め, 終わり, 間隔)のように書きます。"));
                };
                // Both ends included, as the language's `1부터 10까지`.
                let mut items = Vec::new();
                let mut x = start;
                while (step > 0.0 && x <= end) || (step < 0.0 && x >= end) {
                    items.push(V::Num(x));
                    x += step;
                    if items.len() > 1_000_000 {
                        break;
                    }
                }
                Ok(V::List(Rc::new(items)))
            }
            Expr::Var(n) if n == "super" => {
                let Some((name, i)) = self.supers.last().cloned() else {
                    return Err(self.fail(line, "super()는 블록 안에서만 써요.", "super()はブロックの中でだけ使えます。"));
                };
                let parent = self.blocks.get(&name).and_then(|d| d.get(i + 1)).cloned();
                let mut out = String::new();
                if let Some(body) = parent {
                    self.supers.push((name, i + 1));
                    let r = self.nodes(&body, &mut out);
                    self.supers.pop();
                    r?;
                }
                Ok(V::Safe(Rc::from(out)))
            }
            Expr::Attr(obj, method) => {
                let v = self.eval(obj, line)?;
                self.method(&v, method, &args, line)
            }
            _ => Err(self.fail(line, "부를 수 있는 것은 range(), super(), 그리고 사전·글의 메서드뿐이에요.", "呼べるのはrange()、super()、辞書・文字列のメソッドだけです。")),
        }
    }

    fn method(&self, v: &V, name: &str, args: &[V], line: usize) -> Result<V, Fail> {
        let text_arg = |i: usize| args.get(i).and_then(|a| a.as_text()).map(str::to_string);
        Ok(match (v, name) {
            (V::Dict(d), "items") => V::List(Rc::new(d.iter().map(|(k, x)| V::List(Rc::new(vec![k.clone(), x.clone()]))).collect())),
            (V::Dict(d), "keys") => V::List(Rc::new(d.iter().map(|(k, _)| k.clone()).collect())),
            (V::Dict(d), "values") => V::List(Rc::new(d.iter().map(|(_, x)| x.clone()).collect())),
            (V::Dict(_), "get") => match args.first().and_then(|k| v.get(k)) {
                Some(x) => x.clone(),
                None => args.get(1).cloned().unwrap_or(V::Null),
            },
            (V::Str(s) | V::Safe(s), _) => match name {
                "upper" => V::str(&s.to_uppercase()),
                "lower" => V::str(&s.to_lowercase()),
                "strip" | "trim" => V::str(s.trim()),
                "startswith" => V::Bool(text_arg(0).is_some_and(|p| s.starts_with(&p))),
                "endswith" => V::Bool(text_arg(0).is_some_and(|p| s.ends_with(&p))),
                "replace" => match (text_arg(0), text_arg(1)) {
                    (Some(a), Some(b)) => V::str(&s.replace(&a, &b)),
                    _ => return Err(self.fail(line, "replace(찾을 글, 바꿀 글)처럼 써요.", "replace(探す文字列, 置き換える文字列)のように書きます。")),
                },
                "split" => {
                    let parts: Vec<V> = match text_arg(0) {
                        Some(sep) if !sep.is_empty() => s.split(sep.as_str()).map(V::str).collect(),
                        _ => s.split_whitespace().map(V::str).collect(),
                    };
                    V::List(Rc::new(parts))
                }
                _ => return Err(self.unknown_method(name, line)),
            },
            _ => return Err(self.unknown_method(name, line)),
        })
    }

    fn unknown_method(&self, name: &str, line: usize) -> Fail {
        self.fail(line, &format!("이 값에는 {name}() 메서드가 없어요."), &format!("この値には{name}()メソッドがありません。"))
    }

    fn binary(&self, op: Op, a: V, b: V, line: usize) -> Result<V, Fail> {
        use std::cmp::Ordering;
        Ok(match op {
            Op::Eq => V::Bool(same(&a, &b)),
            Op::Ne => V::Bool(!same(&a, &b)),
            Op::Lt | Op::Gt | Op::Le | Op::Ge => {
                let ord = match (&a, &b) {
                    (V::Num(x), V::Num(y)) => x.partial_cmp(y),
                    _ => match (a.as_text(), b.as_text()) {
                        (Some(x), Some(y)) => Some(x.cmp(y)),
                        _ => None,
                    },
                };
                let Some(ord) = ord else {
                    let (x, y) = (a.text(self.lang), b.text(self.lang));
                    return Err(self.fail(line, &format!("크기를 비교할 수 없어요: {x}, {y}"), &format!("大小を比べられません: {x}, {y}")));
                };
                V::Bool(match op {
                    Op::Lt => ord == Ordering::Less,
                    Op::Gt => ord == Ordering::Greater,
                    Op::Le => ord != Ordering::Greater,
                    _ => ord != Ordering::Less,
                })
            }
            Op::In | Op::NotIn => {
                let found = match &b {
                    V::List(items) => items.iter().any(|x| same(x, &a)),
                    V::Dict(_) => b.get(&a).is_some(),
                    V::Str(s) | V::Safe(s) => match a.as_text() {
                        Some(t) => s.contains(t),
                        None => false,
                    },
                    _ => false,
                };
                V::Bool(found == (op == Op::In))
            }
            Op::Concat => V::str(&(a.text(self.lang) + &b.text(self.lang))),
            Op::Add => match (&a, &b) {
                (V::Num(x), V::Num(y)) => V::Num(x + y),
                (V::List(x), V::List(y)) => V::List(Rc::new(x.iter().chain(y.iter()).cloned().collect())),
                _ => match (a.as_text(), b.as_text()) {
                    (Some(x), Some(y)) => V::str(&format!("{x}{y}")),
                    _ => return Err(self.not_number(if matches!(a, V::Num(_)) { &b } else { &a }, line)),
                },
            },
            Op::Mul => match (&a, &b) {
                (V::Num(x), V::Num(y)) => V::Num(x * y),
                (V::Str(s), V::Num(n)) | (V::Num(n), V::Str(s)) if *n >= 0.0 && *n < 100_000.0 => V::str(&s.repeat(*n as usize)),
                _ => return Err(self.not_number(if matches!(a, V::Num(_)) { &b } else { &a }, line)),
            },
            _ => {
                let (V::Num(x), V::Num(y)) = (&a, &b) else {
                    return Err(self.not_number(if matches!(a, V::Num(_)) { &b } else { &a }, line));
                };
                if *y == 0.0 && op != Op::Sub {
                    return Err(self.fail(line, "0으로 나눌 수 없어요.", "0で割ることはできません。"));
                }
                V::Num(match op {
                    Op::Sub => x - y,
                    Op::Div => x / y,
                    Op::FloorDiv => (x / y).floor(),
                    _ => x - y * (x / y).floor(),
                })
            }
        })
    }

    fn test(&self, name: &str, v: &V, args: &[V], line: usize) -> Result<bool, Fail> {
        Ok(match name {
            "defined" => !matches!(v, V::Undef),
            "undefined" => matches!(v, V::Undef),
            "none" => matches!(v, V::Null),
            "number" => matches!(v, V::Num(_)),
            "string" => v.as_text().is_some(),
            "mapping" => matches!(v, V::Dict(_)),
            "sequence" | "iterable" => matches!(v, V::List(_) | V::Str(_) | V::Safe(_) | V::Dict(_)),
            "true" => matches!(v, V::Bool(true)),
            "false" => matches!(v, V::Bool(false)),
            "even" | "odd" | "divisibleby" => {
                let V::Num(n) = v else { return Err(self.not_number(v, line)) };
                let d = match name {
                    "divisibleby" => match args.first() {
                        Some(V::Num(d)) if *d != 0.0 => *d,
                        _ => return Err(self.fail(line, "divisibleby(수)처럼 써요.", "divisibleby(数)のように書きます。")),
                    },
                    _ => 2.0,
                };
                let r = n - d * (n / d).floor();
                if name == "odd" {
                    r != 0.0
                } else {
                    r == 0.0
                }
            }
            "in" => match args.first() {
                Some(c) => self.binary(Op::In, v.clone(), c.clone(), line)?.truthy(),
                None => false,
            },
            _ => return Err(self.fail(line, &format!("알 수 없는 검사예요: {name}"), &format!("知らないテストです: {name}"))),
        })
    }

    fn filter(&self, name: &str, v: V, args: &[V], line: usize) -> Result<V, Fail> {
        let text = |v: &V| v.text(self.lang);
        let arg_text = |i: usize| args.get(i).map(|a| a.text(self.lang));
        let arg_num = |i: usize| match args.get(i) {
            Some(V::Num(n)) => Some(*n),
            _ => None,
        };
        let list = |v: &V| -> Option<Vec<V>> {
            match v {
                V::List(items) => Some(items.iter().cloned().collect()),
                V::Dict(d) => Some(d.iter().map(|(k, _)| k.clone()).collect()),
                V::Str(s) | V::Safe(s) => Some(s.chars().map(|c| V::str(&c.to_string())).collect()),
                V::Undef | V::Null => Some(Vec::new()),
                _ => None,
            }
        };
        let need_list = || {
            let t = text(&v);
            self.fail(line, &format!("{name} 필터에는 목록을 줘요: {t}"), &format!("{name}フィルターにはリストを渡します: {t}"))
        };
        Ok(match name {
            "safe" => V::Safe(Rc::from(text(&v))),
            "escape" | "e" => match v {
                V::Safe(_) => v,
                other => V::Safe(Rc::from(html_escape(&text(&other)))),
            },
            "string" => V::str(&text(&v)),
            "upper" => V::str(&text(&v).to_uppercase()),
            "lower" => V::str(&text(&v).to_lowercase()),
            "trim" => V::str(text(&v).trim()),
            "capitalize" => {
                let t = text(&v).to_lowercase();
                let mut cs = t.chars();
                V::str(&cs.next().map_or(String::new(), |c| c.to_uppercase().chain(cs).collect()))
            }
            "title" => {
                let mut out = String::new();
                let mut start = true;
                for c in text(&v).chars() {
                    if start {
                        out.extend(c.to_uppercase());
                    } else {
                        out.extend(c.to_lowercase());
                    }
                    start = !c.is_alphanumeric();
                }
                V::str(&out)
            }
            "length" | "count" => V::Num(match &v {
                V::Str(s) | V::Safe(s) => s.chars().count(),
                V::List(items) => items.len(),
                V::Dict(d) => d.len(),
                V::Undef | V::Null => 0,
                _ => return Err(need_list()),
            } as f64),
            "default" | "d" => {
                let fallback = args.first().cloned().unwrap_or_else(|| V::str(""));
                let boolean = args.get(1).is_some_and(V::truthy);
                if matches!(v, V::Undef) || (boolean && !v.truthy()) {
                    fallback
                } else {
                    v
                }
            }
            "join" => {
                let items = list(&v).ok_or_else(need_list)?;
                let sep = arg_text(0).unwrap_or_default();
                let texts: Vec<String> = items.iter().map(text).collect();
                V::str(&texts.join(&sep))
            }
            "first" => list(&v).ok_or_else(need_list)?.into_iter().next().unwrap_or(V::Undef),
            "last" => list(&v).ok_or_else(need_list)?.pop().unwrap_or(V::Undef),
            "reverse" => match &v {
                V::Str(s) | V::Safe(s) => V::str(&s.chars().rev().collect::<String>()),
                _ => {
                    let mut items = list(&v).ok_or_else(need_list)?;
                    items.reverse();
                    V::List(Rc::new(items))
                }
            },
            "list" => V::List(Rc::new(list(&v).ok_or_else(need_list)?)),
            "sort" => {
                let mut items = list(&v).ok_or_else(need_list)?;
                let reverse = args.first().is_some_and(V::truthy);
                items.sort_by(key_cmp);
                if reverse {
                    items.reverse();
                }
                V::List(Rc::new(items))
            }
            "unique" => {
                let mut out: Vec<V> = Vec::new();
                for item in list(&v).ok_or_else(need_list)? {
                    if !out.iter().any(|x| same(x, &item)) {
                        out.push(item);
                    }
                }
                V::List(Rc::new(out))
            }
            "sum" | "min" | "max" => {
                let items = list(&v).ok_or_else(need_list)?;
                let mut nums = Vec::new();
                for item in &items {
                    match item {
                        V::Num(n) => nums.push(*n),
                        other => return Err(self.not_number(other, line)),
                    }
                }
                match name {
                    "sum" => V::Num(nums.iter().sum()),
                    "min" => nums.into_iter().reduce(f64::min).map_or(V::Undef, V::Num),
                    _ => nums.into_iter().reduce(f64::max).map_or(V::Undef, V::Num),
                }
            }
            "abs" | "round" | "int" | "float" => {
                let n = match &v {
                    V::Num(n) => *n,
                    V::Str(s) | V::Safe(s) if name == "int" || name == "float" => s.trim().parse::<f64>().unwrap_or(0.0),
                    V::Undef | V::Null if name == "int" || name == "float" => 0.0,
                    other => return Err(self.not_number(other, line)),
                };
                V::Num(match name {
                    "abs" => n.abs(),
                    "int" => n.trunc(),
                    "float" => n,
                    _ => {
                        let p = 10f64.powi(arg_num(0).unwrap_or(0.0) as i32);
                        (n * p).round() / p
                    }
                })
            }
            "replace" => match (arg_text(0), arg_text(1)) {
                (Some(a), Some(b)) => V::str(&text(&v).replace(&a, &b)),
                _ => return Err(self.fail(line, "replace(찾을 글, 바꿀 글)처럼 써요.", "replace(探す文字列, 置き換える文字列)のように書きます。")),
            },
            "truncate" => {
                let n = arg_num(0).unwrap_or(255.0).max(0.0) as usize;
                let end = arg_text(1).unwrap_or_else(|| "...".into());
                let t = text(&v);
                if t.chars().count() <= n {
                    V::str(&t)
                } else {
                    V::str(&(t.chars().take(n).collect::<String>() + &end))
                }
            }
            "wordcount" => V::Num(text(&v).split_whitespace().count() as f64),
            "center" => {
                let width = arg_num(0).unwrap_or(80.0).max(0.0) as usize;
                let t = text(&v);
                let len = t.chars().count();
                if len >= width {
                    V::str(&t)
                } else {
                    let left = (width - len) / 2;
                    V::str(&format!("{}{t}{}", " ".repeat(left), " ".repeat(width - len - left)))
                }
            }
            "indent" => {
                let width = arg_num(0).unwrap_or(4.0).max(0.0) as usize;
                let pad = " ".repeat(width);
                V::str(&text(&v).replace('\n', &format!("\n{pad}")))
            }
            "items" => match &v {
                V::Dict(d) => V::List(Rc::new(d.iter().map(|(k, x)| V::List(Rc::new(vec![k.clone(), x.clone()]))).collect())),
                V::Undef | V::Null => V::List(Rc::new(Vec::new())),
                _ => return Err(self.fail(line, "items 필터에는 사전을 줘요.", "itemsフィルターには辞書を渡します。")),
            },
            "tojson" => match to_json(&v, true) {
                Ok(j) => V::Safe(Rc::from(j)),
                Err(what) => return Err(self.fail(line, &format!("JSON으로 바꿀 수 없는 값이에요: {what}"), &format!("JSONにできない値です: {what}"))),
            },
            "urlencode" => V::str(&percent_encode(&text(&v))),
            "striptags" => {
                let mut out = String::new();
                let mut inside = false;
                for c in text(&v).chars() {
                    match c {
                        '<' => inside = true,
                        '>' if inside => inside = false,
                        c if !inside => out.push(c),
                        _ => {}
                    }
                }
                V::str(&out.split_whitespace().collect::<Vec<_>>().join(" "))
            }
            "nl2br" => V::Safe(Rc::from(html_escape(&text(&v)).replace('\n', "<br>\n"))),
            _ => return Err(self.fail(line, &format!("알 수 없는 필터예요: {name}"), &format!("知らないフィルターです: {name}"))),
        })
    }
}

/// A 1-based index (negative from the end) into a length.
fn pick(len: usize, n: f64) -> Option<usize> {
    if n.fract() != 0.0 {
        return None;
    }
    let k = if n < 0.0 { len as f64 + n } else { n - 1.0 };
    (k >= 0.0 && k < len as f64).then_some(k as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(entries: &[(&str, V)]) -> V {
        V::dict(entries.iter().map(|(k, v)| (V::str(k), v.clone())).collect())
    }

    fn list(items: &[V]) -> V {
        V::List(Rc::new(items.to_vec()))
    }

    fn files<'a>(files: &'a [(&'a str, &'a str)]) -> impl FnMut(&str) -> Result<Rc<Template>, Fail> + 'a {
        move |name| {
            let src = files.iter().find(|(n, _)| *n == name).map(|(_, s)| *s).ok_or_else(|| format!("없음: {name}"))?;
            Ok(Rc::new(parse(src, name, Lang::Hari)?))
        }
    }

    fn render_str(src: &str, v: &V) -> Result<String, Fail> {
        let t = Rc::new(parse(src, "t", Lang::Hari)?);
        render(&t, v, Lang::Hari, &mut files(&[]))
    }

    fn r(src: &str, v: &V) -> String {
        render_str(src, v).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn prints_and_escapes() {
        let v = vars(&[("이름", V::str("<하늘>")), ("n", V::Num(3.0)), ("ok", V::Bool(true)), ("없음", V::Null)]);
        assert_eq!(r("안녕, {{ 이름 }}!", &v), "안녕, &lt;하늘&gt;!");
        assert_eq!(r("{{ 이름|safe }}", &v), "<하늘>");
        assert_eq!(r("{{ n + 1.5 }} {{ ok }} [{{ 없음 }}] [{{ 모름 }}]", &v), "4.5 참 [] []");
        assert_eq!(r("{{ n ~ '개' }} {{ n * 2 }} {{ 7 // 2 }} {{ 7 % 3 }} {{ -n }}", &v), "3개 6 3 1 -3");
        assert_eq!(r("{{ '가' if ok else '나' }} {{ '가' if not ok else '나' }}", &v), "가 나");
    }

    #[test]
    fn if_and_for() {
        let v = vars(&[("xs", list(&[V::Num(1.0), V::Num(2.0), V::Num(3.0)])), ("빈", list(&[]))]);
        assert_eq!(r("{% for x in xs %}{{ loop.index }}:{{ x }}{% if not loop.last %},{% endif %}{% endfor %}", &v), "1:1,2:2,3:3");
        assert_eq!(r("{% for x in 빈 %}{{ x }}{% else %}없음{% endfor %}", &v), "없음");
        assert_eq!(r("{% if xs|length > 2 %}많음{% elif xs %}조금{% else %}없음{% endif %}", &v), "많음");
        assert_eq!(r("{% for i in range(3) %}{{ i }}{% endfor %}", &v), "123");
        assert_eq!(r("{% set 합 = xs|sum %}{{ 합 }}", &v), "6");
        let d = vars(&[("d", vars(&[("b", V::Num(2.0)), ("a", V::Num(1.0))]))]);
        assert_eq!(r("{% for k, v in d.items() %}{{ k }}={{ v }};{% endfor %}", &d), "a=1;b=2;");
        assert_eq!(r("{{ d.a }}{{ d['b'] }}{{ d.get('c', 9) }}", &d), "129");
    }

    #[test]
    fn block_tags_take_their_line() {
        let v = vars(&[("xs", list(&[V::str("a"), V::str("b")]))]);
        let src = "<ul>\n  {% for x in xs %}\n  <li>{{ x }}</li>\n  {% endfor %}\n</ul>\n";
        assert_eq!(r(src, &v), "<ul>\n  <li>a</li>\n  <li>b</li>\n</ul>\n");
        assert_eq!(r("a  {%- if true -%}  b  {%- endif %}", &v), "ab");
        assert_eq!(r("{# 설명 #}x{% raw %}{{ 그대로 }}{% endraw %}", &v), "x{{ 그대로 }}");
    }

    #[test]
    fn indexes_count_from_one() {
        let v = vars(&[("xs", list(&[V::str("a"), V::str("b"), V::str("c")]))]);
        assert_eq!(r("{{ xs[1] }}{{ xs[-1] }}{{ xs.2 }}[{{ xs[4] }}]{{ xs|first }}{{ xs|last }}", &v), "acb[]ac");
    }

    #[test]
    fn filters_and_tests() {
        let v = vars(&[("s", V::str(" hello world ")), ("n", V::Num(2.567))]);
        assert_eq!(r("{{ s|trim|upper }}|{{ s|title|trim }}|{{ n|round(2) }}|{{ n|int }}", &v), "HELLO WORLD|Hello World|2.57|2");
        assert_eq!(r("{{ 모름|default('기본') }}|{{ ''|default('빈', true) }}|{{ [3,1,2]|sort|join(',') }}", &v), "기본|빈|1,2,3");
        assert_eq!(r("{{ 모름 is defined }} {{ n is number }} {{ 4 is even }} {{ 9 is divisibleby 3 }} {{ 1 is not odd }}", &v), "거짓 참 참 참 거짓");
        assert_eq!(r("{{ {'a': [1, '<'] }|tojson }}", &v), "{\"a\":[1,\"\\u003c\"]}");
        assert_eq!(r("{{ '가나다라마'|truncate(2) }} {{ 'a' in 'cat' }} {{ 5 not in [1, 2] }}", &v), "가나... 참 참");
    }

    #[test]
    fn extends_blocks_and_include() {
        let fs = [
            ("base.html", "<title>{% block title %}기본{% endblock %}</title>\n{% block body %}{% endblock %}\n{% include 'foot.html' %}"),
            ("page.html", "{% extends 'base.html' %}\n{% block title %}{{ 제목 }} - {{ super() }}{% endblock %}\n{% block body %}<p>본문</p>{% endblock %}"),
            ("foot.html", "<footer>{{ 제목 }}</footer>"),
        ];
        let mut load = files(&fs);
        let page = load("page.html").unwrap();
        let v = vars(&[("제목", V::str("첫 글"))]);
        let out = render(&page, &v, Lang::Hari, &mut load).unwrap();
        // The newline after `{% endblock %}` goes with the tag.
        assert_eq!(out, "<title>첫 글 - 기본</title>\n<p>본문</p><footer>첫 글</footer>");
    }

    #[test]
    fn errors_say_where() {
        let e = render_str("줄1\n{% if %}", &V::Null).unwrap_err();
        assert!(e.starts_with("t 2번째 줄:"), "{e}");
        let e = render_str("{% for x in xs %}", &V::Null).unwrap_err();
        assert!(e.contains("endfor"), "{e}");
        let e = render_str("{{ 1 + '가' }}", &V::Null).unwrap_err();
        assert!(e.contains("숫자가 아니에요"), "{e}");
        let e = render_str("{% frobnicate %}", &V::Null).unwrap_err();
        assert!(e.contains("알 수 없는 태그"), "{e}");
        let e = render_str("{{ x|없는필터 }}", &V::Null).unwrap_err();
        assert!(e.contains("알 수 없는 필터"), "{e}");
    }
}
