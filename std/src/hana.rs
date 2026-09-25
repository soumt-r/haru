//! Hana's argument helpers (`std/stdimpl/stdimpl.go`): each standard
//! function checks its own arguments and fails with Hana's error codes, so
//! the messages are Hana's word for word.

use haru_sdk::prelude::*;

/// 2^53: past it a number stops holding every whole number.
const MAX_SAFE_INTEGER: f64 = 9007199254740992.0;

pub fn exactly(args: &[Value], n: usize) -> Result<()> {
    if args.len() != n {
        return Err(Error::new("ArgumentError.ArgCountExact").arg(n as f64));
    }
    Ok(())
}

pub fn between(args: &[Value], min: usize, max: usize) -> Result<()> {
    if args.len() < min || args.len() > max {
        return Err(Error::new("ArgumentError.ArgCountRange").arg(min as f64).arg(max as f64));
    }
    Ok(())
}

/// `args[i]` as a number, or `NativeArgNumber` naming position i+1.
pub fn number(args: &[Value], i: usize) -> Result<f64> {
    args[i].as_num().ok_or_else(|| Error::new("TypeError.NativeArgNumber").arg((i + 1) as f64))
}

/// `args[i]` as a whole number within ±2^53.
pub fn integer(args: &[Value], i: usize) -> Result<i64> {
    match args[i].as_num() {
        Some(n) if n == n.trunc() && n.abs() <= MAX_SAFE_INTEGER => Ok(n as i64),
        _ => Err(Error::new("TypeError.NativeArgInteger").arg((i + 1) as f64)),
    }
}

/// -0 as 0, so every engine prints the same.
pub fn clean(f: f64) -> f64 {
    if f == 0.0 {
        0.0
    } else {
        f
    }
}

/// A result that must be a real number (`MathDomain` otherwise).
pub fn finite(f: f64) -> Result<Value> {
    if f.is_nan() || f.is_infinite() {
        return Err(Error::new("ValueError.MathDomain"));
    }
    Ok(Value::num(clean(f)))
}
