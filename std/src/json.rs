//! [JSON] / 【JSON】, as Hana implements it (`std/stdimpl/json.go`). Reading
//! follows Go's `encoding/json` into `interface{}`: the same grammar, the same
//! nesting limit, lone surrogates as U+FFFD, numbers out of range rejected.

use haru_sdk::abi::tag;
use haru_sdk::prelude::*;
use haru_sdk::IntoRet;

use crate::hana::{between, exactly, integer, string};

haru_sdk::entry!(pub(crate) fn entry = "json", build);

fn build(m: &mut Module) {
    crate::describe(m, "json", &[("json.parse", parse), ("json.stringify", stringify)]);
}

fn invalid() -> Error {
    Error::new("ValueError.JSONInvalid")
}

fn unsupported() -> Error {
    Error::new("ValueError.JSONUnsupported")
}

fn parse(args: &[Value]) -> Result<Value> {
    exactly(args, 1)?;
    let text = string(args, 0)?;
    Reader { b: text.as_bytes(), i: 0 }.document().ok_or_else(invalid)?
}

/// Go's scanner allows this many open arrays and objects at once.
const MAX_NESTING: usize = 10000;

enum Open {
    List(Vec<Value>),
    Dict(Vec<(String, Value)>, String),
}

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl Reader<'_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Option<()> {
        (self.peek() == Some(c)).then(|| self.i += 1)
    }

    /// The whole text as one value: `None` when it is not JSON. The inner
    /// `Result` is a failure of the host while building values.
    fn document(&mut self) -> Option<Result<Value>> {
        let mut stack: Vec<Open> = Vec::new();
        loop {
            self.space();
            let mut v = match self.peek()? {
                b'[' => {
                    self.i += 1;
                    if stack.len() + 1 > MAX_NESTING {
                        return None;
                    }
                    self.space();
                    if self.eat(b']').is_some() {
                        List::new().into_ret()
                    } else {
                        stack.push(Open::List(Vec::new()));
                        continue;
                    }
                }
                b'{' => {
                    self.i += 1;
                    if stack.len() + 1 > MAX_NESTING {
                        return None;
                    }
                    self.space();
                    if self.eat(b'}').is_some() {
                        Dict::new().into_ret()
                    } else {
                        let key = self.key()?;
                        stack.push(Open::Dict(Vec::new(), key));
                        continue;
                    }
                }
                b'"' => Ok(Value::str(&self.string()?)),
                b't' => self.word("true", Value::bool(true))?,
                b'f' => self.word("false", Value::bool(false))?,
                b'n' => self.word("null", Value::NULL)?,
                b'-' | b'0'..=b'9' => Ok(Value::num(self.number()?)),
                _ => return None,
            };
            // Hand the value to what is open, closing what it ends.
            loop {
                let value = match v {
                    Ok(value) => value,
                    Err(e) => return Some(Err(e)),
                };
                match stack.last_mut() {
                    None => {
                        self.space();
                        return (self.i == self.b.len()).then_some(Ok(value));
                    }
                    Some(Open::List(items)) => {
                        items.push(value);
                        self.space();
                        match self.peek()? {
                            b',' => {
                                self.i += 1;
                                break;
                            }
                            b']' => {
                                self.i += 1;
                                let Some(Open::List(items)) = stack.pop() else { unreachable!() };
                                v = new_list(items);
                            }
                            _ => return None,
                        }
                    }
                    Some(Open::Dict(entries, key)) => {
                        entries.push((std::mem::take(key), value));
                        self.space();
                        match self.peek()? {
                            b',' => {
                                self.i += 1;
                                self.space();
                                let next = self.key()?;
                                if let Some(Open::Dict(_, key)) = stack.last_mut() {
                                    *key = next;
                                }
                                break;
                            }
                            b'}' => {
                                self.i += 1;
                                let Some(Open::Dict(entries, _)) = stack.pop() else { unreachable!() };
                                v = new_dict(entries);
                            }
                            _ => return None,
                        }
                    }
                }
            }
        }
    }

    fn word(&mut self, w: &str, v: Value) -> Option<Result<Value>> {
        self.b[self.i..].starts_with(w.as_bytes()).then(|| {
            self.i += w.len();
            Ok(v)
        })
    }

    /// `"key"` and the colon after it.
    fn key(&mut self) -> Option<String> {
        if self.peek()? != b'"' {
            return None;
        }
        let k = self.string()?;
        self.space();
        self.eat(b':')?;
        Some(k)
    }

    fn hex4(&self, at: usize) -> Option<u32> {
        let h = self.b.get(at..at + 4)?;
        let s = std::str::from_utf8(h).ok()?;
        if !s.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        u32::from_str_radix(s, 16).ok()
    }

    fn string(&mut self) -> Option<String> {
        self.i += 1;
        let mut out = String::new();
        loop {
            let c = self.peek()?;
            match c {
                b'"' => {
                    self.i += 1;
                    return Some(out);
                }
                b'\\' => {
                    let e = *self.b.get(self.i + 1)?;
                    self.i += 2;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let r = self.hex4(self.i)?;
                            self.i += 4;
                            if (0xd800..0xe000).contains(&r) {
                                // A pair only when a low half follows at once.
                                let low = (self.b.get(self.i) == Some(&b'\\') && self.b.get(self.i + 1) == Some(&b'u'))
                                    .then(|| self.hex4(self.i + 2))
                                    .flatten();
                                match low {
                                    Some(lo) if r < 0xdc00 && (0xdc00..0xe000).contains(&lo) => {
                                        self.i += 6;
                                        out.push(char::from_u32(0x10000 + ((r - 0xd800) << 10) + (lo - 0xdc00)).unwrap());
                                    }
                                    _ => out.push('\u{fffd}'),
                                }
                            } else {
                                out.push(char::from_u32(r).unwrap());
                            }
                        }
                        _ => return None,
                    }
                }
                0..=0x1f => return None,
                _ => {
                    // A whole character (the text is valid UTF-8).
                    let len = match c {
                        0..=0x7f => 1,
                        0xc0..=0xdf => 2,
                        0xe0..=0xef => 3,
                        _ => 4,
                    };
                    out.push_str(std::str::from_utf8(&self.b[self.i..self.i + len]).ok()?);
                    self.i += len;
                }
            }
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.i;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.i += 1;
        }
        self.i - start
    }

    fn number(&mut self) -> Option<f64> {
        let start = self.i;
        self.eat(b'-');
        match self.peek()? {
            b'0' => self.i += 1,
            b'1'..=b'9' => {
                self.digits();
            }
            _ => return None,
        }
        if self.eat(b'.').is_some() && self.digits() == 0 {
            return None;
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if self.digits() == 0 {
                return None;
            }
        }
        let text = std::str::from_utf8(&self.b[start..self.i]).ok()?;
        // Go refuses a number too large for a float64.
        text.parse::<f64>().ok().filter(|f| f.is_finite())
    }
}

fn new_list(items: Vec<Value>) -> Result<Value> {
    let list = List::new();
    for v in items {
        list.push(v)?;
    }
    list.into_ret()
}

fn new_dict(entries: Vec<(String, Value)>) -> Result<Value> {
    let dict = Dict::new();
    for (k, v) in entries {
        dict.set(k, v)?;
    }
    dict.into_ret()
}

/// A value as JSON: dictionary keys must be text and are written sorted; the
/// optional second argument is the indent per level (0 for one line).
fn stringify(args: &[Value]) -> Result<Value> {
    between(args, 1, 2)?;
    let mut indent = 0;
    if args.len() == 2 {
        let n = integer(args, 1)?;
        if !(0..=10).contains(&n) {
            return Err(Error::new("TypeError.NativeArgInteger").arg(2.0));
        }
        indent = n as usize;
    }
    let mut out = String::new();
    write(&mut out, &args[0], indent, 0)?;
    Ok(Value::str(&out))
}

/// Stops a list that contains itself from being written for ever.
const MAX_DEPTH: usize = 1000;

fn write(out: &mut String, v: &Value, indent: usize, depth: usize) -> Result<()> {
    match v.tag() {
        tag::NULL => out.push_str("null"),
        tag::BOOL => out.push_str(if v.as_bool().unwrap() { "true" } else { "false" }),
        tag::NUM => {
            let f = v.as_num().unwrap();
            if !f.is_finite() {
                return Err(unsupported());
            }
            out.push_str(&number(f));
        }
        tag::STR => quote(out, &v.as_str().unwrap()),
        tag::LIST => {
            if depth > MAX_DEPTH {
                return Err(unsupported());
            }
            let list = v.as_list().unwrap();
            if list.is_empty() {
                out.push_str("[]");
                return Ok(());
            }
            out.push('[');
            for (i, item) in list.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline(out, indent, depth + 1);
                write(out, &item, indent, depth + 1)?;
            }
            newline(out, indent, depth);
            out.push(']');
        }
        tag::DICT => {
            // Hana has no limit here (a dictionary inside itself overflows
            // Go's stack); this one only keeps Haru from crashing.
            if depth > MAX_DEPTH {
                return Err(unsupported());
            }
            let dict = v.as_dict().unwrap();
            if dict.is_empty() {
                out.push_str("{}");
                return Ok(());
            }
            let mut keys = Vec::new();
            for k in dict.keys().iter() {
                keys.push((k.as_str().ok_or_else(unsupported)?, k));
            }
            keys.sort_by(|a, b| (*a.0).cmp(&*b.0));
            out.push('{');
            for (i, (name, k)) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline(out, indent, depth + 1);
                quote(out, name);
                out.push(':');
                if indent > 0 {
                    out.push(' ');
                }
                write(out, &dict.get(k).unwrap_or(Value::NULL), indent, depth + 1)?;
            }
            newline(out, indent, depth);
            out.push('}');
        }
        _ => return Err(unsupported()),
    }
    Ok(())
}

fn newline(out: &mut String, indent: usize, depth: usize) {
    if indent > 0 {
        out.push('\n');
        out.push_str(&" ".repeat(indent * depth));
    }
}

/// JavaScript's number to text: plain digits between 1e-6 and 1e21, an
/// exponent outside, "0" for -0.
pub(crate) fn number(f: f64) -> String {
    if f == 0.0 {
        return "0".into();
    }
    let abs = f.abs();
    if !(1e-6..1e21).contains(&abs) {
        let s = format!("{f:e}");
        let (mantissa, exp) = s.split_once('e').unwrap();
        return match exp.strip_prefix('-') {
            Some(d) => format!("{mantissa}e-{d}"),
            None => format!("{mantissa}e+{exp}"),
        };
    }
    format!("{f}")
}

/// Quoted as JSON.stringify does: only ", \ and control characters escaped.
fn quote(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}
