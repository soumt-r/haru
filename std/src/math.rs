//! [수학] / 【数学】, as Hana implements it (`std/stdimpl/mathstats.go`).

use haru_sdk::prelude::*;

use crate::hana::{between, clean, exactly, finite, integer, number};

haru_sdk::entry!(pub(crate) fn entry = "math", build);

fn build(m: &mut Module) {
    crate::describe(m, "math", &[
        ("math.ceil", ceil),
        ("math.floor", floor),
        ("math.sqrt", |a| one(a, f64::sqrt)),
        ("math.pow", pow),
        ("math.abs", |a| one(a, f64::abs)),
        ("math.round", round),
        ("math.sin", |a| one(a, f64::sin)),
        ("math.cos", |a| one(a, f64::cos)),
        ("math.tan", |a| one(a, f64::tan)),
        ("math.log", log),
        ("math.pi", pi),
        ("math.gcd", gcd),
        ("math.factorial", factorial),
    ]);
}

/// Hana's `numberFunc`: `NotANumber` for anything else, and the result as it
/// comes (-0 stays -0).
fn number_func(args: &[Value], f: fn(f64) -> f64) -> Result<Value> {
    exactly(args, 1)?;
    match args[0].as_num() {
        Some(n) => Ok(Value::num(f(n))),
        None => Err(Error::new("TypeError.NotANumber")),
    }
}

fn ceil(args: &[Value]) -> Result<Value> {
    number_func(args, f64::ceil)
}

fn floor(args: &[Value]) -> Result<Value> {
    number_func(args, f64::floor)
}

/// Hana's `one`: a function of one number that may leave its domain.
fn one(args: &[Value], f: fn(f64) -> f64) -> Result<Value> {
    exactly(args, 1)?;
    finite(f(number(args, 0)?))
}

fn pow(args: &[Value]) -> Result<Value> {
    exactly(args, 2)?;
    let (base, exp) = (number(args, 0)?, number(args, 1)?);
    finite(base.powf(exp))
}

/// Half away from zero, to a whole number or to 0..15 digits.
fn round(args: &[Value]) -> Result<Value> {
    between(args, 1, 2)?;
    let x = number(args, 0)?;
    let mut digits = 0;
    if args.len() == 2 {
        digits = integer(args, 1)?;
        if !(0..=15).contains(&digits) {
            return Err(Error::new("TypeError.NativeArgInteger").arg(2.0));
        }
    }
    let mut scale = 1.0;
    for _ in 0..digits {
        scale *= 10.0;
    }
    finite((x * scale).round() / scale)
}

/// The natural logarithm, or the logarithm in a base.
fn log(args: &[Value]) -> Result<Value> {
    between(args, 1, 2)?;
    let x = number(args, 0)?;
    if x <= 0.0 {
        return Err(Error::new("ValueError.MathDomain"));
    }
    if args.len() == 1 {
        return finite(x.ln());
    }
    let base = number(args, 1)?;
    if base <= 0.0 || base == 1.0 {
        return Err(Error::new("ValueError.MathDomain"));
    }
    finite(x.ln() / base.ln())
}

fn pi(args: &[Value]) -> Result<Value> {
    exactly(args, 0)?;
    Ok(Value::num(std::f64::consts::PI))
}

fn gcd(args: &[Value]) -> Result<Value> {
    exactly(args, 2)?;
    let (mut a, mut b) = (integer(args, 0)?.abs(), integer(args, 1)?.abs());
    while b != 0 {
        (a, b) = (b, a % b);
    }
    Ok(Value::num(clean(a as f64)))
}

/// n! for whole n from 0 up to 170 (the last one a number holds).
fn factorial(args: &[Value]) -> Result<Value> {
    exactly(args, 1)?;
    let n = integer(args, 0)?;
    if !(0..=170).contains(&n) {
        return Err(Error::new("ValueError.MathDomain"));
    }
    let mut result = 1.0;
    for i in 2..=n {
        result *= i as f64;
    }
    Ok(Value::num(result))
}
