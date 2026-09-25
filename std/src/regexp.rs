//! [정규식] / 【正規表現】, as Hana implements it (`std/stdimpl/regex.go`):
//! Go's syntax and matching rules (see `goregex.rs`). Empty matches are
//! ignored by 모두찾기, 치환 and 분할.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use haru_sdk::prelude::*;
use regex::{Regex, RegexBuilder};

use crate::hana::{exactly, new_list, string};

haru_sdk::entry!(pub(crate) fn entry = "regex", build);

fn build(m: &mut Module) {
    crate::describe(m, "regex", &[
        ("regex.test", test),
        ("regex.find", find),
        ("regex.groups", groups),
        ("regex.findall", find_all),
        ("regex.replace", replace),
        ("regex.split", split),
    ]);
}

thread_local! {
    /// Patterns compiled lately: a program usually uses a few, many times.
    static CACHE: RefCell<HashMap<String, Rc<Regex>>> = RefCell::new(HashMap::new());
}

const CACHE_SIZE: usize = 64;

/// Patterns nesting deeper than this are compiled on a thread with room for it.
const DEEP: usize = 150;

fn compile(pattern: &str) -> Result<Rc<Regex>> {
    if let Some(re) = CACHE.with(|c| c.borrow().get(pattern).cloned()) {
        return Ok(re);
    }
    let invalid = || Error::new("ValueError.RegexInvalid").arg(pattern);
    let t = crate::goregex::translate(pattern).ok_or_else(invalid)?;
    let build = move || {
        RegexBuilder::new(&t.pattern)
            .size_limit(1 << 30)
            .dfa_size_limit(64 << 20)
            .nest_limit(u32::MAX)
            .build()
            .ok()
    };
    let re = if t.height > DEEP {
        std::thread::Builder::new()
            .stack_size(256 << 20)
            .spawn(build)
            .ok()
            .and_then(|h| h.join().ok())
            .flatten()
    } else {
        build()
    };
    let re = Rc::new(re.ok_or_else(invalid)?);
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() >= CACHE_SIZE {
            c.clear();
        }
        c.insert(pattern.to_string(), re.clone());
    });
    Ok(re)
}

/// The (text, pattern) every function starts with.
fn text_and_pattern(args: &[Value], n: usize) -> Result<(Str, Rc<Regex>)> {
    exactly(args, n)?;
    let text = string(args, 0)?;
    let pattern = string(args, 1)?;
    Ok((text, compile(&pattern)?))
}

fn test(args: &[Value]) -> Result<Value> {
    let (text, re) = text_and_pattern(args, 2)?;
    Ok(Value::bool(re.is_match(&text)))
}

fn find(args: &[Value]) -> Result<Value> {
    let (text, re) = text_and_pattern(args, 2)?;
    Ok(re.find(&text).map_or(Value::NULL, |m| Value::str(m.as_str())))
}

/// Each group of a match: (start, end), or None when it took no part.
type Groups = Vec<Option<(usize, usize)>>;

fn group_values(text: &str, g: &Groups) -> Vec<Value> {
    g.iter().map(|x| x.map_or(Value::NULL, |(s, e)| Value::str(&text[s..e]))).collect()
}

/// The first match: the whole of it, then each group (비어있음 when a group
/// took no part).
fn groups(args: &[Value]) -> Result<Value> {
    let (text, re) = text_and_pattern(args, 2)?;
    let mut locs = re.capture_locations();
    if re.captures_read(&mut locs, &text).is_none() {
        return Ok(Value::NULL);
    }
    let g: Groups = (0..locs.len()).map(|i| locs.get(i)).collect();
    new_list(group_values(&text, &g))
}

/// The non-empty matches, the way Go's `FindAll...` walks the text.
fn matches(re: &Regex, text: &str) -> Vec<Groups> {
    let mut out = Vec::new();
    let mut locs = re.capture_locations();
    let end = text.len();
    let mut pos = 0;
    while pos <= end {
        let Some(m) = re.captures_read_at(&mut locs, text, pos) else { break };
        if m.end() == pos {
            // An empty match: go on from the next character.
            pos += text[pos..].chars().next().map_or(1, char::len_utf8);
        } else {
            pos = m.end();
        }
        if m.end() > m.start() {
            out.push((0..locs.len()).map(|i| locs.get(i)).collect());
        }
    }
    out
}

fn find_all(args: &[Value]) -> Result<Value> {
    let (text, re) = text_and_pattern(args, 2)?;
    let found = matches(&re, &text).into_iter().map(|g| {
        let (s, e) = g[0].unwrap();
        Value::str(&text[s..e])
    });
    new_list(found.collect::<Vec<_>>())
}

/// Every match swapped for the replacement, where $1..$9 (or $10...) are its
/// groups and $$ is a dollar sign.
fn replace(args: &[Value]) -> Result<Value> {
    let (text, re) = text_and_pattern(args, 3)?;
    let replacement = string(args, 2)?;
    let mut out = String::new();
    let mut last = 0;
    for g in matches(&re, &text) {
        let (s, e) = g[0].unwrap();
        out.push_str(&text[last..s]);
        expand(&mut out, replacement.as_bytes(), &text, &g);
        last = e;
    }
    out.push_str(&text[last..]);
    Ok(Value::str(&out))
}

/// Hana's `expandReplacement`, byte for byte.
fn expand(out: &mut String, r: &[u8], text: &str, g: &Groups) {
    let mut bytes = Vec::with_capacity(r.len());
    let mut i = 0;
    while i < r.len() {
        let c = r[i];
        if c != b'$' || i + 1 >= r.len() {
            bytes.push(c);
            i += 1;
            continue;
        }
        let next = r[i + 1];
        if next == b'$' {
            bytes.push(b'$');
            i += 2;
            continue;
        }
        if !next.is_ascii_digit() {
            bytes.push(c);
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < r.len() && r[j].is_ascii_digit() {
            j += 1;
        }
        // Go's int wraps; a number this long names no group either way.
        let n = r[i + 1..j].iter().fold(0usize, |n, d| n.wrapping_mul(10).wrapping_add((d - b'0') as usize));
        if n < g.len() {
            if let Some((s, e)) = g[n] {
                bytes.extend_from_slice(&text.as_bytes()[s..e]);
            }
        } else {
            bytes.extend_from_slice(&r[i..j]);
        }
        i = j;
    }
    // The pieces are whole characters of valid text.
    out.push_str(&String::from_utf8(bytes).unwrap_or_default());
}

fn split(args: &[Value]) -> Result<Value> {
    let (text, re) = text_and_pattern(args, 2)?;
    let mut out = Vec::new();
    let mut last = 0;
    for g in matches(&re, &text) {
        let (s, e) = g[0].unwrap();
        out.push(Value::str(&text[last..s]));
        last = e;
    }
    out.push(Value::str(&text[last..]));
    new_list(out)
}
