//! [텍스트] / 【テキスト】, as Hana implements it (`std/stdimpl/text.go`).

use haru_sdk::prelude::*;

use crate::hana::{exactly, integer, list, string, MAX_RESULT};

haru_sdk::entry!(pub(crate) fn entry = "text", build);

fn build(m: &mut Module) {
    crate::describe(m, "text", &[
        ("text.upper", |a| text1(a, upper)),
        ("text.lower", |a| text1(a, lower)),
        ("text.strip", |a| text1(a, |s| s.trim_matches(STRIP).to_string())),
        ("text.padleft", |a| pad(a, true)),
        ("text.padright", |a| pad(a, false)),
        ("text.repeat", repeat),
        ("text.reverse", |a| text1(a, |s| s.chars().rev().collect())),
        ("text.startswith", |a| two(a, |t, p| Ok(Value::bool(t.starts_with(p))))),
        ("text.endswith", |a| two(a, |t, p| Ok(Value::bool(t.ends_with(p))))),
        ("text.join", join),
        ("text.count", |a| two(a, count)),
        ("text.find", |a| two(a, find)),
    ]);
}

/// Spaces, tabs, line breaks, the no-break space and the ideographic space.
const STRIP: &[char] = &[' ', '\t', '\n', '\u{b}', '\u{c}', '\r', '\u{a0}', '\u{3000}'];

fn text1(args: &[Value], f: fn(&str) -> String) -> Result<Value> {
    exactly(args, 1)?;
    Ok(Value::str(&f(&string(args, 0)?)))
}

/// Go's `unicode.ToUpper`: one character for one (the simple mapping).
pub(crate) fn upper(s: &str) -> String {
    s.chars()
        .map(|c| {
            let mut up = c.to_uppercase();
            match (up.next(), up.next()) {
                (Some(u), None) => u,
                // Rust gives the full mapping; these have a simple one too.
                _ => match c as u32 {
                    0x1f80..=0x1f87 | 0x1f90..=0x1f97 | 0x1fa0..=0x1fa7 => char::from_u32(c as u32 + 8).unwrap(),
                    0x1fb3 => '\u{1fbc}',
                    0x1fc3 => '\u{1fcc}',
                    0x1ff3 => '\u{1ffc}',
                    _ => c,
                },
            }
        })
        .collect()
}

/// Go's `unicode.ToLower`.
pub(crate) fn lower(s: &str) -> String {
    s.chars()
        .map(|c| {
            let mut low = c.to_lowercase();
            match (low.next(), low.next()) {
                (Some(l), None) => l,
                _ if c == '\u{130}' => 'i',
                _ => c,
            }
        })
        .collect()
}

/// (text, width, fill): the text with fill added up to width characters.
fn pad(args: &[Value], left: bool) -> Result<Value> {
    exactly(args, 3)?;
    let text = string(args, 0)?;
    let width = integer(args, 1)?;
    let fill = string(args, 2)?;
    if fill.chars().count() != 1 {
        return Err(Error::new("ValueError.PadFillLength"));
    }
    if !(0..=MAX_RESULT).contains(&width) {
        return Err(Error::new("ValueError.ResultTooLarge"));
    }
    let missing = (width - text.chars().count() as i64).max(0) as usize;
    let filler = fill.repeat(missing);
    Ok(Value::str(&if left { filler + &text } else { text.to_string() + &filler }))
}

fn repeat(args: &[Value]) -> Result<Value> {
    exactly(args, 2)?;
    let text = string(args, 0)?;
    let n = integer(args, 1)?;
    if n < 0 {
        return Err(Error::new("TypeError.NativeArgInteger").arg(2.0));
    }
    if (text.chars().count() as i64).saturating_mul(n) > MAX_RESULT {
        return Err(Error::new("ValueError.ResultTooLarge"));
    }
    Ok(Value::str(&text.repeat(n as usize)))
}

fn two(args: &[Value], f: fn(&str, &str) -> Result<Value>) -> Result<Value> {
    exactly(args, 2)?;
    let text = string(args, 0)?;
    let part = string(args, 1)?;
    f(&text, &part)
}

fn join(args: &[Value]) -> Result<Value> {
    exactly(args, 2)?;
    let items = list(args, 0)?;
    let sep = string(args, 1)?;
    let mut parts = Vec::with_capacity(items.len());
    for v in items.iter() {
        match v.as_str() {
            Some(s) => parts.push(s),
            None => return Err(Error::new("TypeError.NativeListStrings").arg(1.0)),
        }
    }
    let parts: Vec<&str> = parts.iter().map(|s| &**s).collect();
    Ok(Value::str(&parts.join(&sep)))
}

/// How many times the part occurs, without overlapping.
fn count(text: &str, part: &str) -> Result<Value> {
    if part.is_empty() {
        return Err(Error::new("ValueError.TextEmptyPart"));
    }
    Ok(Value::num(text.matches(part).count() as f64))
}

/// The 1-based position of the first occurrence, or 비어있음.
fn find(text: &str, part: &str) -> Result<Value> {
    if part.is_empty() {
        return Err(Error::new("ValueError.TextEmptyPart"));
    }
    Ok(match text.find(part) {
        Some(i) => Value::num((text[..i].chars().count() + 1) as f64),
        None => Value::NULL,
    })
}
