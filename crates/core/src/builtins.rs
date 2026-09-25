//! The functions every program has without importing (spec 5.5) and the
//! string methods, as Hana implements them (`stdlib/builtins.go`,
//! `vm/eval_expr.go`).

use haru_abi::tag;

use crate::error::RuntimeError;
use crate::format::display;
use crate::lang::Lang;
use crate::value::Value;
use crate::vm::codes::*;

/// `lang.builtins[i]`: 문자로, 숫자로, 코드로, 글자로.
pub fn call(id: u8, args: &[Value], lang: &Lang) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(RuntimeError::core(ARG_COUNT).num_arg(1.0));
    }
    let a = &args[0];
    match id {
        0 => Ok(Value::string(display(a, lang))),
        1 => match a.tag() {
            tag::NUM => Ok(a.clone()),
            tag::STR => {
                let s = a.as_str().unwrap();
                go_parse_float(s)
                    .map(Value::num)
                    .ok_or_else(|| RuntimeError::core(TO_NUMBER_FAILED).str_arg(s))
            }
            _ => Err(RuntimeError::core(TO_NUMBER_INVALID)),
        },
        2 => {
            let mut chars = a.as_str().map(|s| s.chars());
            match chars.as_mut().and_then(|c| c.next().filter(|_| c.next().is_none())) {
                Some(c) => Ok(Value::num(c as u32 as f64)),
                None => Err(RuntimeError::core(TO_CODE).str_arg(lang.builtins[2])),
            }
        }
        _ => match a.as_num() {
            // Go: string(rune(f)); an invalid code point becomes U+FFFD.
            Some(n) => {
                let code = go_rune(n);
                let c = u32::try_from(code).ok().and_then(char::from_u32).unwrap_or('\u{FFFD}');
                Ok(Value::string(c.to_string()))
            }
            None => Err(RuntimeError::core(TO_TEXT)),
        },
    }
}

/// Go's `rune(f)` (a float64 converted to int32 through int64 on amd64).
fn go_rune(n: f64) -> i32 {
    crate::format::go_i64(n) as i32
}

/// `<자르기>`, `<바꾸기>`, `<분리하기>`, `<포함확인>`.
pub fn string_method(s: &str, name: &str, args: &[Value], lang: &Lang) -> Result<Value, RuntimeError> {
    let count = |n: usize| {
        if args.len() != n {
            Err(RuntimeError::core(ARG_COUNT).num_arg(n as f64))
        } else {
            Ok(())
        }
    };
    let text = |i: usize| args[i].as_str().ok_or_else(|| RuntimeError::core(METHOD_ARG_STRING).str_arg(name));
    if name == lang.string_slice {
        count(2)?;
        let (Some(start), Some(end)) = (args[0].as_num(), args[1].as_num()) else {
            return Err(RuntimeError::core(METHOD_ARG_NUMBER).str_arg(name));
        };
        let chars: Vec<char> = s.chars().collect();
        let mut start = (crate::format::go_int(start) - 1).max(0);
        let end = crate::format::go_int(end).min(chars.len() as i64);
        if start > end {
            start = end;
        }
        let (start, end) = (start.max(0) as usize, end.max(0) as usize);
        return Ok(Value::string(chars[start.min(end)..end].iter().collect()));
    }
    if name == lang.string_replace {
        count(2)?;
        let (old, new) = (text(0)?, text(1)?);
        return Ok(Value::string(s.replace(old, new)));
    }
    if name == lang.string_split {
        count(1)?;
        let sep = text(0)?;
        let parts: Vec<Value> = if sep.is_empty() {
            // Go splits into characters ("" into nothing at all).
            s.chars().map(|c| Value::string(c.to_string())).collect()
        } else {
            s.split(sep).map(Value::str).collect()
        };
        return Ok(Value::list(parts));
    }
    if name == lang.string_contains {
        count(1)?;
        return Ok(Value::bool(s.contains(text(0)?)));
    }
    Err(RuntimeError::core(METHOD_NOT_FOUND).str_arg(name))
}

/// `^[+-]?(\d+(\.\d*)?|\.\d+)$`: what `입력받자` accepts as a number.
pub fn plain_number(s: &str) -> bool {
    let b = s.strip_prefix(['+', '-']).unwrap_or(s).as_bytes();
    let int = b.iter().take_while(|c| c.is_ascii_digit()).count();
    let rest = &b[int..];
    if int > 0 {
        match rest.split_first() {
            None => true,
            Some((b'.', frac)) => frac.iter().all(u8::is_ascii_digit),
            _ => false,
        }
    } else {
        matches!(rest.split_first(), Some((b'.', frac)) if !frac.is_empty() && frac.iter().all(u8::is_ascii_digit))
    }
}

/// Go's `strconv.ParseFloat(s, 64)`: decimal (with `_` between digits),
/// hexadecimal with a `p` exponent, `inf`/`infinity`/`nan`; out of range fails.
pub fn go_parse_float(s: &str) -> Option<f64> {
    let (neg, body) = match s.as_bytes().first() {
        Some(b'+') => (false, &s[1..]),
        Some(b'-') => (true, &s[1..]),
        _ => (false, s),
    };
    let sign = if neg { -1.0 } else { 1.0 };
    let lower = body.to_ascii_lowercase();
    if lower == "inf" || lower == "infinity" {
        return Some(sign * f64::INFINITY);
    }
    if lower == "nan" {
        return Some(f64::NAN);
    }
    if let Some(hex) = lower.strip_prefix("0x") {
        return parse_hex(hex).map(|v| sign * v);
    }
    // Underscores only between digits.
    let bytes = body.as_bytes();
    for (i, &c) in bytes.iter().enumerate() {
        if c == b'_' {
            let ok = i > 0 && bytes[i - 1].is_ascii_digit() && bytes.get(i + 1).is_some_and(u8::is_ascii_digit);
            if !ok {
                return None;
            }
        }
    }
    let clean: String = body.chars().filter(|&c| c != '_').collect();
    let (mantissa, exp) = match clean.find(['e', 'E']) {
        Some(i) => (&clean[..i], Some(&clean[i + 1..])),
        None => (clean.as_str(), None),
    };
    if !plain_number(mantissa) || mantissa.starts_with(['+', '-']) {
        return None;
    }
    if let Some(e) = exp {
        let digits = e.strip_prefix(['+', '-']).unwrap_or(e);
        if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
    }
    let v: f64 = clean.parse().ok()?;
    if v.is_infinite() {
        return None;
    }
    Some(sign * v)
}

fn parse_hex(s: &str) -> Option<f64> {
    let p = s.find('p')?;
    let (mant, exp) = (&s[..p], &s[p + 1..]);
    let exp: i32 = exp.parse().ok()?;
    let mut value = 0f64;
    let mut scale = 0i32;
    let mut seen_dot = false;
    let mut digits = 0;
    let mant: String = mant.chars().filter(|&c| c != '_').collect();
    for c in mant.chars() {
        if c == '.' {
            if seen_dot {
                return None;
            }
            seen_dot = true;
            continue;
        }
        let d = c.to_digit(16)?;
        value = value * 16.0 + d as f64;
        digits += 1;
        if seen_dot {
            scale -= 4;
        }
    }
    if digits == 0 {
        return None;
    }
    let v = value * 2f64.powi(exp + scale);
    v.is_finite().then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_float_like_go() {
        for (s, want) in [("1_000", Some(1000.0)), ("0x1p-2", Some(0.25)), ("1e3", Some(1000.0)), (".5", Some(0.5)), ("5.", Some(5.0)), ("+5", Some(5.0)), ("0x_1p0", Some(1.0))] {
            assert_eq!(go_parse_float(s), want, "{s}");
        }
        for s in ["0x10", "1e400", " 5", "5 ", "", "0b101", "1__0", "_1", "1e", "e3", "--1"] {
            assert_eq!(go_parse_float(s), None, "{s}");
        }
        assert!(go_parse_float("-INF").unwrap().is_infinite());
        assert!(go_parse_float("nan").unwrap().is_nan());
    }
}
