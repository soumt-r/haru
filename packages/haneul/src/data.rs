//! Plain data taken out of the program's values: what templates compute
//! with, what JSON reads and writes, and what path arguments hold.

use std::cmp::Ordering;
use std::rc::Rc;

use haru_sdk::abi::tag;
use haru_sdk::prelude::*;
use haru_sdk::IntoRet;

/// The program's language: which words values print with and which locale
/// error messages come in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Hari,
    Kanade,
}

impl Lang {
    /// The locale `Value::call_catching` takes.
    pub fn locale(self) -> u32 {
        match self {
            Lang::Hari => 1,
            Lang::Kanade => 2,
        }
    }

    pub fn tr(self, ko: &str, ja: &str) -> String {
        match self {
            Lang::Hari => ko.to_string(),
            Lang::Kanade => ja.to_string(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum V {
    /// A template variable that does not exist.
    Undef,
    Null,
    Bool(bool),
    Num(f64),
    Str(Rc<str>),
    /// Text that is HTML already (`|safe`): printed without escaping.
    Safe(Rc<str>),
    List(Rc<Vec<V>>),
    /// Entries sorted by key (the program's dictionaries keep no order).
    Dict(Rc<Vec<(V, V)>>),
    /// What templates can only print (a function, an object).
    Opaque(Rc<str>),
}

impl V {
    pub fn str(s: &str) -> V {
        V::Str(Rc::from(s))
    }

    pub fn dict(mut entries: Vec<(V, V)>) -> V {
        entries.sort_by(|a, b| key_cmp(&a.0, &b.0));
        entries.dedup_by(|b, a| same(&a.0, &b.0));
        V::Dict(Rc::new(entries))
    }

    pub fn truthy(&self) -> bool {
        match self {
            V::Undef | V::Null => false,
            V::Bool(b) => *b,
            V::Num(n) => *n != 0.0,
            V::Str(s) | V::Safe(s) => !s.is_empty(),
            V::List(l) => !l.is_empty(),
            V::Dict(d) => !d.is_empty(),
            V::Opaque(_) => true,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            V::Str(s) | V::Safe(s) => Some(s),
            _ => None,
        }
    }

    /// The value under a key of a dictionary.
    pub fn get(&self, key: &V) -> Option<&V> {
        match self {
            V::Dict(d) => d.iter().find(|(k, _)| same(k, key)).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn get_str(&self, key: &str) -> Option<&V> {
        match self {
            V::Dict(d) => d.iter().find(|(k, _)| k.as_text() == Some(key)).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The value as the program prints it (`비어있음` prints nothing here).
    pub fn text(&self, lang: Lang) -> String {
        let mut out = String::new();
        write_text(&mut out, self, lang, 0);
        out
    }
}

fn write_text(out: &mut String, v: &V, lang: Lang, depth: usize) {
    match v {
        V::Undef | V::Null if depth == 0 => {}
        V::Undef | V::Null => out.push_str(match lang {
            Lang::Hari => "비어있음",
            Lang::Kanade => "空っぽ",
        }),
        V::Bool(b) => out.push_str(match (lang, b) {
            (Lang::Hari, true) => "참",
            (Lang::Hari, false) => "거짓",
            (Lang::Kanade, true) => "真",
            (Lang::Kanade, false) => "偽",
        }),
        V::Num(n) => out.push_str(&number(*n)),
        V::Str(s) | V::Safe(s) | V::Opaque(s) => out.push_str(s),
        V::List(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_text(out, item, lang, depth + 1);
            }
            out.push(']');
        }
        V::Dict(entries) => {
            out.push('{');
            for (i, (k, item)) in entries.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_text(out, k, lang, depth + 1);
                out.push_str(": ");
                write_text(out, item, lang, depth + 1);
            }
            out.push('}');
        }
    }
}

/// Whether two values are equal (texts by content, lists and dictionaries
/// item by item).
pub fn same(a: &V, b: &V) -> bool {
    match (a, b) {
        (V::Undef | V::Null, V::Undef | V::Null) => true,
        (V::Bool(x), V::Bool(y)) => x == y,
        (V::Num(x), V::Num(y)) => x == y,
        (V::List(x), V::List(y)) => x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| same(p, q)),
        (V::Dict(x), V::Dict(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| same(&p.0, &q.0) && same(&p.1, &q.1))
        }
        _ => match (a.as_text(), b.as_text()) {
            (Some(x), Some(y)) => x == y,
            _ => false,
        },
    }
}

/// The order of dictionary keys: numbers, then texts, then the rest.
pub fn key_cmp(a: &V, b: &V) -> Ordering {
    fn rank(v: &V) -> u8 {
        match v {
            V::Num(_) => 0,
            V::Str(_) | V::Safe(_) => 1,
            _ => 2,
        }
    }
    match (a, b) {
        (V::Num(x), V::Num(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        _ => match (a.as_text(), b.as_text()) {
            (Some(x), Some(y)) => x.cmp(y),
            _ => rank(a).cmp(&rank(b)),
        },
    }
}

/// A number as the program prints it (the shortest decimal, no exponent).
pub fn number(n: f64) -> String {
    if n.is_nan() {
        "NaN".to_string()
    } else if n.is_infinite() {
        if n > 0.0 { "+Inf" } else { "-Inf" }.to_string()
    } else if n == 0.0 {
        if n.is_sign_negative() { "-0" } else { "0" }.to_string()
    } else if n.fract() == 0.0 && n.abs() < 1e15 {
        (n as i64).to_string()
    } else {
        format!("{n}")
    }
}

const MAX_DEPTH: usize = 100;

/// The plain data of a program value.
pub fn from_value(v: &Value, lang: Lang) -> V {
    from_value_at(v, lang, 0)
}

fn from_value_at(v: &Value, lang: Lang, depth: usize) -> V {
    match v.tag() {
        tag::NULL => V::Null,
        tag::BOOL => V::Bool(v.as_bool().unwrap_or(false)),
        tag::NUM => V::Num(v.as_num().unwrap_or(0.0)),
        tag::STR => V::str(&v.as_str().unwrap()),
        _ if depth > MAX_DEPTH => V::Opaque(Rc::from("[...]")),
        tag::LIST => V::List(Rc::new(v.as_list().unwrap().iter().map(|x| from_value_at(&x, lang, depth + 1)).collect())),
        tag::DICT => {
            let d = v.as_dict().unwrap();
            let entries = d
                .keys()
                .iter()
                .map(|k| {
                    let item = d.get(&k).unwrap_or(Value::NULL);
                    (from_value_at(&k, lang, depth + 1), from_value_at(&item, lang, depth + 1))
                })
                .collect();
            V::dict(entries)
        }
        tag::FUNC => V::Opaque(Rc::from(lang.tr("[함수]", "[関数]"))),
        _ => V::Opaque(Rc::from(lang.tr("[객체]", "[オブジェクト]"))),
    }
}

/// A new program value holding the data.
pub fn to_value(v: &V) -> Result<Value> {
    Ok(match v {
        V::Undef | V::Null => Value::NULL,
        V::Bool(b) => Value::bool(*b),
        V::Num(n) => Value::num(*n),
        V::Str(s) | V::Safe(s) | V::Opaque(s) => Value::str(s),
        V::List(items) => {
            let list = List::new();
            for item in items.iter() {
                list.push(to_value(item)?)?;
            }
            list.into_ret()?
        }
        V::Dict(entries) => {
            let d = Dict::new();
            for (k, item) in entries.iter() {
                d.set(to_value(k)?, to_value(item)?)?;
            }
            d.into_ret()?
        }
    })
}

// ---------------------------------------------------------------------------
// JSON

/// JSON text of the data; `Err` holds what cannot be written. With
/// `html_safe`, `<`, `>`, `&` and `'` are escaped so the text can sit in a page.
pub fn to_json(v: &V, html_safe: bool) -> std::result::Result<String, String> {
    let mut out = String::new();
    write_json(&mut out, v, html_safe, 0)?;
    Ok(out)
}

fn write_json(out: &mut String, v: &V, html_safe: bool, depth: usize) -> std::result::Result<(), String> {
    if depth > MAX_DEPTH {
        return Err("[...]".into());
    }
    match v {
        V::Undef | V::Null => out.push_str("null"),
        V::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        V::Num(n) if n.is_finite() => out.push_str(&number(if *n == 0.0 { 0.0 } else { *n })),
        V::Num(n) => return Err(number(*n)),
        V::Str(s) | V::Safe(s) => quote(out, s, html_safe),
        V::Opaque(s) => return Err(s.to_string()),
        V::List(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(out, item, html_safe, depth + 1)?;
            }
            out.push(']');
        }
        V::Dict(entries) => {
            out.push('{');
            for (i, (k, item)) in entries.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                let key = match k {
                    V::Num(n) => number(*n),
                    other => other.as_text().ok_or_else(|| other.text(Lang::Hari))?.to_string(),
                };
                quote(out, &key, html_safe);
                out.push(':');
                write_json(out, item, html_safe, depth + 1)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

fn quote(out: &mut String, s: &str, html_safe: bool) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<' | '>' | '&' | '\'' if html_safe => out.push_str(&format!("\\u{:04x}", c as u32)),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The data of a JSON text, or `None` when it is not JSON.
pub fn parse_json(text: &str) -> Option<V> {
    let mut r = Reader { b: text.as_bytes(), i: 0 };
    r.space();
    let v = r.value(0)?;
    r.space();
    (r.i == r.b.len()).then_some(v)
}

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl Reader<'_> {
    fn space(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        self.space();
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn word(&mut self, w: &str) -> bool {
        if self.b[self.i..].starts_with(w.as_bytes()) {
            self.i += w.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Option<V> {
        if depth > 1000 {
            return None;
        }
        self.space();
        match *self.b.get(self.i)? {
            b'n' => self.word("null").then_some(V::Null),
            b't' => self.word("true").then_some(V::Bool(true)),
            b'f' => self.word("false").then_some(V::Bool(false)),
            b'"' => self.string().map(|s| V::str(&s)),
            b'[' => {
                self.i += 1;
                let mut items = Vec::new();
                if !self.eat(b']') {
                    loop {
                        items.push(self.value(depth + 1)?);
                        if self.eat(b']') {
                            break;
                        }
                        if !self.eat(b',') {
                            return None;
                        }
                    }
                }
                Some(V::List(Rc::new(items)))
            }
            b'{' => {
                self.i += 1;
                let mut entries = Vec::new();
                if !self.eat(b'}') {
                    loop {
                        self.space();
                        if self.b.get(self.i) != Some(&b'"') {
                            return None;
                        }
                        let k = self.string()?;
                        if !self.eat(b':') {
                            return None;
                        }
                        let item = self.value(depth + 1)?;
                        // A later key wins.
                        entries.retain(|(x, _): &(V, V)| x.as_text() != Some(k.as_str()));
                        entries.push((V::str(&k), item));
                        if self.eat(b'}') {
                            break;
                        }
                        if !self.eat(b',') {
                            return None;
                        }
                    }
                }
                Some(V::dict(entries))
            }
            _ => self.number(),
        }
    }

    fn number(&mut self) -> Option<V> {
        let start = self.i;
        let digits = |r: &mut Self| {
            let s = r.i;
            while r.i < r.b.len() && r.b[r.i].is_ascii_digit() {
                r.i += 1;
            }
            r.i > s
        };
        if self.b.get(self.i) == Some(&b'-') {
            self.i += 1;
        }
        if self.b.get(self.i) == Some(&b'0') {
            self.i += 1;
        } else if !digits(self) {
            return None;
        }
        if self.b.get(self.i) == Some(&b'.') {
            self.i += 1;
            if !digits(self) {
                return None;
            }
        }
        if matches!(self.b.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.b.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if !digits(self) {
                return None;
            }
        }
        let n: f64 = std::str::from_utf8(&self.b[start..self.i]).ok()?.parse().ok()?;
        n.is_finite().then_some(V::Num(n))
    }

    fn hex4(&mut self) -> Option<u32> {
        let h = std::str::from_utf8(self.b.get(self.i..self.i + 4)?).ok()?;
        let n = u32::from_str_radix(h, 16).ok()?;
        self.i += 4;
        Some(n)
    }

    fn string(&mut self) -> Option<String> {
        self.i += 1;
        let mut out = String::new();
        loop {
            let start = self.i;
            while self.i < self.b.len() && self.b[self.i] != b'"' && self.b[self.i] != b'\\' && self.b[self.i] >= 0x20 {
                self.i += 1;
            }
            out.push_str(std::str::from_utf8(&self.b[start..self.i]).ok()?);
            match *self.b.get(self.i)? {
                b'"' => {
                    self.i += 1;
                    return Some(out);
                }
                b'\\' => {
                    self.i += 1;
                    let c = *self.b.get(self.i)?;
                    self.i += 1;
                    match c {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let u = self.hex4()?;
                            let c = if (0xd800..0xdc00).contains(&u) && self.b[self.i..].starts_with(b"\\u") {
                                let save = self.i;
                                self.i += 2;
                                match self.hex4() {
                                    Some(l) if (0xdc00..0xe000).contains(&l) => {
                                        char::from_u32(0x10000 + ((u - 0xd800) << 10) + (l - 0xdc00))
                                    }
                                    _ => {
                                        self.i = save;
                                        None
                                    }
                                }
                            } else {
                                char::from_u32(u)
                            };
                            out.push(c.unwrap_or('\u{fffd}'));
                        }
                        _ => return None,
                    }
                }
                _ => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_reads_and_writes() {
        let v = parse_json(r#" {"b": [1, 2.5, true, null], "a": "\uD55C\ud558\"<"} "#).unwrap();
        assert_eq!(to_json(&v, false).unwrap(), r#"{"a":"한하\"<","b":[1,2.5,true,null]}"#);
        assert_eq!(to_json(&v, true).unwrap(), "{\"a\":\"한하\\\"\\u003c\",\"b\":[1,2.5,true,null]}");
        assert!(parse_json("[1,]").is_none());
        assert!(parse_json("01").is_none());
        assert!(parse_json("").is_none());
    }

    #[test]
    fn numbers_print_as_the_program_prints_them() {
        assert_eq!(number(3.0), "3");
        assert_eq!(number(0.1 + 0.2), "0.30000000000000004");
        assert_eq!(number(1e21), "1000000000000000000000");
    }
}
