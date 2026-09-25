//! [경로] / 【パス】, as Hana implements it (`std/stdimpl/path.go`): the text
//! of a path only, never the disk. "\" counts as "/", a letter and a colon at
//! the start is a drive, and results use "/".

use haru_sdk::prelude::*;

use crate::hana::{between, exactly, new_list, string};

haru_sdk::entry!(pub(crate) fn entry = "path", build);

fn build(m: &mut Module) {
    crate::describe(m, "path", &[
        ("path.join", join),
        ("path.dirname", |a| one(a, dirname)),
        ("path.basename", |a| one(a, |p| Ok(Value::str(base_name(p))))),
        ("path.ext", |a| one(a, |p| Ok(Value::str(split_ext(base_name(p)).1)))),
        ("path.stem", |a| one(a, |p| Ok(Value::str(split_ext(base_name(p)).0)))),
        ("path.withext", with_ext),
        ("path.normalize", |a| one(a, normalize)),
        ("path.isabs", |a| one(a, |p| Ok(Value::bool(split(p).1.starts_with('/'))))),
        ("path.parts", |a| one(a, parts)),
    ]);
}

/// The drive ("" or "C:") and the rest, with "\" turned into "/".
fn split(p: &str) -> (String, String) {
    let p = p.replace('\\', "/");
    let b = p.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        return (p[..2].to_string(), p[2..].to_string());
    }
    (String::new(), p)
}

fn one(args: &[Value], f: fn(&str) -> Result<Value>) -> Result<Value> {
    exactly(args, 1)?;
    f(&string(args, 0)?)
}

const MAX_JOIN_PARTS: usize = 100;

/// Parts glued with "/", empty ones skipped; an absolute part starts over.
fn join(args: &[Value]) -> Result<Value> {
    between(args, 1, MAX_JOIN_PARTS)?;
    let mut result = String::new();
    for i in 0..args.len() {
        let (drive, rest) = split(&string(args, i)?);
        let part = drive + &rest;
        if part.is_empty() {
        } else if rest.starts_with('/') || result.is_empty() {
            result = part;
        } else if result.ends_with('/') {
            result += &part;
        } else {
            result.push('/');
            result += &part;
        }
    }
    Ok(Value::str(&result))
}

fn dirname(p: &str) -> Result<Value> {
    let (drive, rest) = split(p);
    let Some(i) = rest.rfind('/') else { return Ok(Value::str(&drive)) };
    let mut head = rest[..=i].trim_end_matches('/');
    if head.is_empty() {
        head = "/";
    }
    Ok(Value::str(&(drive + head)))
}

fn base_name(p: &str) -> &str {
    // "\" is "/", and a drive is always followed by the rest.
    let rest = match p.as_bytes() {
        [d, b':', ..] if d.is_ascii_alphabetic() => &p[2..],
        _ => p,
    };
    match rest.rfind(['/', '\\']) {
        Some(i) => &rest[i + 1..],
        None => rest,
    }
}

/// A file name split at its last dot, ignoring the dots it starts with
/// (".bashrc" has no extension).
fn split_ext(name: &str) -> (&str, &str) {
    let lead = name.len() - name.trim_start_matches('.').len();
    match name.rfind('.') {
        Some(j) if j >= lead => (&name[..j], &name[j..]),
        _ => (name, ""),
    }
}

/// The extension of the last part replaced ("" removes it); a path ending in
/// "/" has no file name and comes back as it is.
fn with_ext(args: &[Value]) -> Result<Value> {
    exactly(args, 2)?;
    let p = string(args, 0)?;
    let ext = string(args, 1)?;
    let (drive, rest) = split(&p);
    let dir_end = rest.rfind('/').map_or(0, |i| i + 1);
    let (dir, base) = rest.split_at(dir_end);
    if base.is_empty() {
        return Ok(Value::str(&(drive + &rest)));
    }
    let (stem, _) = split_ext(base);
    let ext = if !ext.is_empty() && !ext.starts_with('.') { format!(".{}", &*ext) } else { ext.to_string() };
    Ok(Value::str(&format!("{drive}{dir}{stem}{ext}")))
}

/// "//", "." and "x/.." folded; leading ".." stay in a relative path and
/// vanish at the root of an absolute one.
fn normalize(p: &str) -> Result<Value> {
    let (drive, rest) = split(p);
    let absolute = rest.starts_with('/');
    let mut stack: Vec<&str> = Vec::new();
    for seg in rest.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if stack.last().is_some_and(|s| *s != "..") {
                    stack.pop();
                } else if !absolute {
                    stack.push("..");
                }
            }
            _ => stack.push(seg),
        }
    }
    let mut result = stack.join("/");
    if absolute {
        result.insert(0, '/');
    }
    if result.is_empty() {
        return Ok(Value::str(if drive.is_empty() { "." } else { &drive }));
    }
    Ok(Value::str(&(drive + &result)))
}

/// The root first ("/" or "C:/") when absolute, then every name; empty parts
/// and "." dropped, ".." kept.
fn parts(p: &str) -> Result<Value> {
    let (drive, rest) = split(p);
    let mut out = Vec::new();
    if rest.starts_with('/') {
        out.push(Value::str(&(drive + "/")));
    } else if !drive.is_empty() {
        out.push(Value::str(&drive));
    }
    for seg in rest.split('/') {
        if !seg.is_empty() && seg != "." {
            out.push(Value::str(seg));
        }
    }
    new_list(out)
}
