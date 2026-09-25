//! [인코딩] / 【エンコード】 and [해시] / 【ハッシュ】, as Hana implements them
//! (`std/stdimpl/encoding.go`). Text is UTF-8: base64 and URL escaping work
//! on its bytes.

use haru_sdk::prelude::*;
use sha2::{Digest, Sha256};

use crate::hana::{exactly, string};

haru_sdk::entry!(pub(crate) fn entry = "encoding", build);
haru_sdk::entry!(pub(crate) fn hash_entry = "hash", build_hash);

fn build(m: &mut Module) {
    crate::describe(m, "encoding", &[
        ("encoding.base64encode", |a| text1(a, base64_encode)),
        ("encoding.base64decode", base64_decode),
        ("encoding.urlencode", |a| text1(a, url_encode)),
        ("encoding.urldecode", url_decode),
    ]);
}

fn build_hash(m: &mut Module) {
    crate::describe(m, "hash", &[("hash.sha256", |a| text1(a, |s| hex(&Sha256::digest(s.as_bytes()))))]);
}

fn text1(args: &[Value], f: fn(&str) -> String) -> Result<Value> {
    exactly(args, 1)?;
    Ok(Value::str(&f(&string(args, 0)?)))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len().div_ceil(3) * 4);
    for chunk in s.as_bytes().chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Standard base64 with padding and no line breaks, whose bytes are UTF-8
/// text. Like Go's decoder, bits left over in the last character are ignored.
fn base64_decode(args: &[Value]) -> Result<Value> {
    exactly(args, 1)?;
    let s = string(args, 0)?;
    let invalid = || Error::new("ValueError.Base64Invalid");
    let body = s.trim_end_matches('=');
    let pad = s.len() - body.len();
    if s.len() % 4 != 0 || pad > 2 {
        return Err(invalid());
    }
    let mut bytes = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0);
    for c in body.bytes() {
        let v = B64.iter().position(|&x| x == c).ok_or_else(invalid)? as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    // A last group of one character, or padding past a full group, is not base64.
    if body.len() % 4 == 1 || (pad > 0 && body.len() % 4 + pad != 4) {
        return Err(invalid());
    }
    String::from_utf8(bytes).map(|t| Value::str(&t)).map_err(|_| invalid())
}

/// Everything but letters, digits and - _ . ~ as %XX (uppercase).
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn url_decode(args: &[Value]) -> Result<Value> {
    exactly(args, 1)?;
    let s = string(args, 0)?;
    let invalid = || Error::new("ValueError.URLDecodeInvalid");
    let b = s.as_bytes();
    let mut raw = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'%' {
            raw.push(b[i]);
            i += 1;
            continue;
        }
        // Hana wants two more characters and one past them (a quirk kept).
        if i + 2 >= b.len() {
            return Err(invalid());
        }
        let hi = (b[i + 1] as char).to_digit(16).ok_or_else(invalid)?;
        let lo = (b[i + 2] as char).to_digit(16).ok_or_else(invalid)?;
        raw.push((hi * 16 + lo) as u8);
        i += 3;
    }
    String::from_utf8(raw).map(|t| Value::str(&t)).map_err(|_| invalid())
}
