//! `include/haru.h`, the ABI for C (and C++, Zig, ...), is generated from
//! `src/lib.rs`: this test fails when the file is not what the Rust
//! definitions say. `HARU_BLESS=1 cargo test -p haru-abi` writes it anew.
//!
//! The reader knows only the shapes `src/lib.rs` uses (constants in
//! `mod tag` / `mod kind`, `#[repr(C)]` structs of plain fields and
//! `unsafe extern "C" fn` pointers, and `pub type` aliases), and says so
//! when it meets anything else.

use std::fmt::Write as _;

const SOURCE: &str = include_str!("../src/lib.rs");
const HEADER: &str = "include/haru.h";

/// A Rust type of the ABI as C writes it before a name (`uint32_t`,
/// `const uint8_t *`, `HaruStr`).
fn c_type(t: &str) -> String {
    let t = t.trim();
    if let Some(rest) = t.strip_prefix("*const ") {
        return format!("const {}*", c_type(rest));
    }
    if let Some(rest) = t.strip_prefix("*mut ") {
        return format!("{}*", c_type(rest));
    }
    match t {
        "u8" => "uint8_t ".into(),
        "u32" => "uint32_t ".into(),
        "u64" => "uint64_t ".into(),
        "i64" => "int64_t ".into(),
        "usize" => "size_t ".into(),
        "bool" => "bool ".into(),
        "c_void" => "void ".into(),
        "Status" => "HaruStatus ".into(),
        "NativeFn" => "HaruNativeFn ".into(),
        "EntryFn" => "HaruEntryFn ".into(),
        name if name.chars().all(|c| c.is_ascii_alphanumeric()) && name.starts_with(char::is_uppercase) => format!("Haru{name} "),
        other => panic!("haru.h: no C type for `{other}`"),
    }
}

/// `unsafe extern "C" fn(a: A, b: B) -> R` as a C declaration of `name`
/// (a function pointer).
fn c_fn(sig: &str, name: &str) -> String {
    let sig = sig.trim().strip_prefix("unsafe extern \"C\" fn").unwrap_or_else(|| panic!("haru.h: not a C function pointer: {sig}"));
    let open = sig.find('(').unwrap();
    let close = sig.rfind(')').unwrap();
    let params: Vec<String> = sig[open + 1..close]
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (n, t) = p.split_once(':').unwrap();
            format!("{}{}", c_type(t), n.trim())
        })
        .collect();
    let ret = match sig[close + 1..].trim().strip_prefix("->") {
        Some(r) => c_type(r),
        None => "void ".into(),
    };
    let params = if params.is_empty() { "void".into() } else { params.join(", ") };
    format!("{ret}(*{name})({params})")
}

/// A field's name in C: C++ keywords get a trailing `_` (the layout goes by
/// position, so the name is free).
fn c_name(name: &str) -> String {
    match name {
        "throw" | "template" | "class" | "new" | "delete" | "operator" => format!("{name}_"),
        _ => name.to_string(),
    }
}

/// A declaration of field (or alias) `name` of Rust type `t`.
fn c_decl(t: &str, name: &str) -> String {
    if t.trim().starts_with("unsafe extern") {
        c_fn(t, name)
    } else {
        format!("{}{name}", c_type(t))
    }
}

fn comment(out: &mut String, doc: &[String], indent: &str) {
    for line in doc {
        let text = line.replace("[`", "`").replace("`]", "`").replace("`](", "` (");
        if text.is_empty() {
            writeln!(out, "{indent}//").unwrap();
        } else {
            writeln!(out, "{indent}// {text}").unwrap();
        }
    }
}

/// Everything up to the `,` (or `;`) that ends a field or alias, over lines.
fn take_until_end(lines: &[&str], i: &mut usize, first: &str, end: char) -> String {
    let mut text = first.to_string();
    let depth = |s: &str| s.matches('(').count() as i64 - s.matches(')').count() as i64;
    while depth(&text) > 0 || !text.trim_end().ends_with(end) {
        *i += 1;
        text.push(' ');
        text.push_str(lines[*i].trim());
    }
    text.trim_end().trim_end_matches(end).to_string()
}

fn generate() -> String {
    let lines: Vec<&str> = SOURCE.lines().collect();
    let mut out = String::new();
    out.push_str(PREAMBLE);
    // The typedefs of every struct first, so any may point at any.
    for l in &lines {
        if let Some(name) = l.trim().strip_prefix("pub struct ").and_then(|s| s.strip_suffix(" {")) {
            writeln!(out, "typedef struct Haru{name} Haru{name};").unwrap();
        }
    }
    out.push('\n');
    let mut doc: Vec<String> = Vec::new();
    let mut module = "";
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        i += 1;
        if let Some(d) = line.strip_prefix("///") {
            doc.push(d.trim().to_string());
            continue;
        }
        if line.starts_with("//") || line.starts_with("#") || line.is_empty() {
            if line.is_empty() {
                doc.clear();
            }
            continue;
        }
        if let Some(m) = line.strip_prefix("pub mod ").and_then(|m| m.strip_suffix(" {")) {
            module = if m == "tag" { "TAG_" } else if m == "kind" { "KIND_" } else { panic!("haru.h: unknown module {m}") };
            comment(&mut out, &doc, "");
            doc.clear();
            continue;
        }
        if line == "}" {
            if !module.is_empty() {
                module = "";
                out.push('\n');
            }
            doc.clear();
            continue;
        }
        if let Some(rest) = line.strip_prefix("pub const ").filter(|r| !r.starts_with("fn ")) {
            // `NAME: TYPE = VALUE;`
            let (name, rest) = rest.split_once(':').unwrap();
            let (ty, value) = rest.split_once('=').unwrap();
            let value = value.trim().trim_end_matches(';');
            let (name, ty) = (name.trim(), ty.trim());
            match ty {
                "RawValue" | "Str" => {}
                _ => {
                    comment(&mut out, &doc, "");
                    let name = name.strip_prefix("STATUS_").map_or_else(|| format!("{module}{name}"), |n| format!("STATUS_{n}"));
                    writeln!(out, "#define HARU_{name} {value}").unwrap();
                    if module.is_empty() {
                        out.push('\n');
                    }
                }
            }
            doc.clear();
            continue;
        }
        if line.starts_with("pub const fn") || line.starts_with("pub fn") || line.starts_with("pub unsafe fn") || line.starts_with("impl ") || line.starts_with("unsafe impl") || line.starts_with("const _") || line.starts_with("use ") {
            // Rust-only helpers: skip the whole item.
            if line.ends_with('{') {
                let mut depth = 1;
                while depth > 0 {
                    let l = lines[i];
                    depth += l.matches('{').count() as i64 - l.matches('}').count() as i64;
                    i += 1;
                }
            }
            doc.clear();
            continue;
        }
        if let Some(rest) = line.strip_prefix("pub type ") {
            let text = take_until_end(&lines, &mut (i - 1), rest, ';');
            // Advance past the lines take_until_end read.
            while !lines[i - 1].trim_end().ends_with(';') {
                i += 1;
            }
            let (name, ty) = text.split_once('=').unwrap();
            comment(&mut out, &doc, "");
            writeln!(out, "typedef {};\n", c_decl(ty, &format!("Haru{}", name.trim()))).unwrap();
            doc.clear();
            continue;
        }
        if let Some(name) = line.strip_prefix("pub struct ").and_then(|s| s.strip_suffix(" {")) {
            comment(&mut out, &doc, "");
            doc.clear();
            let mut fields = String::new();
            let mut fdoc: Vec<String> = Vec::new();
            let mut opaque = false;
            loop {
                let l = lines[i].trim();
                i += 1;
                if l == "}" {
                    break;
                }
                if let Some(d) = l.strip_prefix("///") {
                    fdoc.push(d.trim().to_string());
                    continue;
                }
                if l.is_empty() {
                    fields.push('\n');
                    continue;
                }
                if l.starts_with("_private") {
                    opaque = true;
                    continue;
                }
                let rest = l.strip_prefix("pub ").unwrap_or_else(|| panic!("haru.h: unexpected field `{l}` in {name}"));
                let mut j = i - 1;
                let text = take_until_end(&lines, &mut j, rest, ',');
                i = j + 1;
                let (fname, ty) = text.split_once(':').unwrap();
                comment(&mut fields, &fdoc, "    ");
                fdoc.clear();
                writeln!(fields, "    {};", c_decl(ty, &c_name(fname.trim()))).unwrap();
            }
            // (Every struct's typedef is at the top.)
            if !opaque {
                writeln!(out, "struct Haru{name} {{\n{fields}}};\n").unwrap();
            }
            continue;
        }
        panic!("haru.h: `{line}` is a shape the generator does not know");
    }
    out.push_str(POSTAMBLE);
    out
}

const PREAMBLE: &str = "\
// haru.h: the Haru native module ABI, version 1, for C (and C++, Zig, ...).
//
// GENERATED from crates/abi/src/lib.rs by crates/abi/tests/header.rs; do not
// edit. `HARU_BLESS=1 cargo test -p haru-abi` writes it anew.
//
// A module is a shared library (.dll, .so, .dylib) that exports one function,
// `const HaruModuleDesc *haru_module_v1_<id>(const HaruHostApi *host)`, and a
// `haru.toml` naming it (`[native] id = \"<id>\"`). See examples/greet_c.

#ifndef HARU_H
#define HARU_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>

#ifdef __cplusplus
extern \"C\" {
#endif

";

const POSTAMBLE: &str = "\
// ---- conveniences (not part of the Rust definitions)

#if defined(__cplusplus)
static_assert(sizeof(HaruRawValue) == 16, \"a value is 16 bytes\");
#else
_Static_assert(sizeof(HaruRawValue) == 16, \"a value is 16 bytes\");
#endif

// Marks the entry function as exported from the shared library.
#if defined(_WIN32)
#define HARU_EXPORT __declspec(dllexport)
#else
#define HARU_EXPORT __attribute__((visibility(\"default\")))
#endif

// A string literal as a HaruStr (`HARU_STR(\"ceil\")`).
// The same as an initializer, for static tables (`HaruName n = { HARU_STR_INIT(\"hari\"), ... }`).
#define HARU_STR_INIT(s) { (const uint8_t *)(s), sizeof(s) - 1 }
#ifdef __cplusplus
#define HARU_STR(s) (HaruStr{ (const uint8_t *)(s), sizeof(s) - 1 })
#else
#define HARU_STR(s) ((HaruStr){ (const uint8_t *)(s), sizeof(s) - 1 })
#endif

static inline HaruRawValue haru_null(void) {
    HaruRawValue v = { HARU_TAG_NULL, 0, 0 };
    return v;
}

static inline HaruRawValue haru_bool(bool b) {
    HaruRawValue v = { HARU_TAG_BOOL, 0, b ? 1u : 0u };
    return v;
}

static inline HaruRawValue haru_num(double n) {
    HaruRawValue v = { HARU_TAG_NUM, 0, 0 };
    memcpy(&v.payload, &n, sizeof n);
    return v;
}

// The number a HARU_TAG_NUM value holds.
static inline double haru_as_num(HaruRawValue v) {
    double n;
    memcpy(&n, &v.payload, sizeof n);
    return n;
}

#ifdef __cplusplus
}
#endif

#endif
";

#[test]
fn the_header_is_current() {
    let want = generate();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(HEADER);
    if std::env::var_os("HARU_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &want).unwrap();
        return;
    }
    let have = std::fs::read_to_string(&path).unwrap_or_default().replace("\r\n", "\n");
    assert!(have == want, "{HEADER} is not what src/lib.rs says: run `HARU_BLESS=1 cargo test -p haru-abi`");
}
