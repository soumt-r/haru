//! [통계] / 【統計】, as Hana implements it (`std/stdimpl/mathstats.go`).

use haru_sdk::prelude::*;

use crate::hana::{clean, exactly, finite};

haru_sdk::entry!(pub(crate) fn entry = "stats", build);

fn build(m: &mut Module) {
    crate::describe(m, "stats", &[
        ("stats.sum", sum),
        ("stats.min", |a| extreme(a, |x, best| x < best)),
        ("stats.max", |a| extreme(a, |x, best| x > best)),
        ("stats.mean", mean),
        ("stats.median", median),
        ("stats.stdev", stdev),
    ]);
}

/// The one argument, a list of numbers.
fn numbers(args: &[Value]) -> Result<Vec<f64>> {
    exactly(args, 1)?;
    let list = crate::hana::list(args, 0)?;
    list.iter()
        .map(|v| v.as_num().ok_or_else(|| Error::new("TypeError.NativeListNumbers").arg(1.0)))
        .collect()
}

fn not_enough() -> Error {
    Error::new("ValueError.StatsNotEnough")
}

fn total(xs: &[f64]) -> f64 {
    xs.iter().fold(0.0, |t, x| t + x)
}

fn sum(args: &[Value]) -> Result<Value> {
    finite(total(&numbers(args)?))
}

fn extreme(args: &[Value], better: fn(f64, f64) -> bool) -> Result<Value> {
    let xs = numbers(args)?;
    let (&first, rest) = xs.split_first().ok_or_else(not_enough)?;
    let best = rest.iter().fold(first, |best, &x| if better(x, best) { x } else { best });
    Ok(Value::num(clean(best)))
}

fn mean(args: &[Value]) -> Result<Value> {
    let xs = numbers(args)?;
    if xs.is_empty() {
        return Err(not_enough());
    }
    finite(total(&xs) / xs.len() as f64)
}

fn median(args: &[Value]) -> Result<Value> {
    let mut xs = numbers(args)?;
    if xs.is_empty() {
        return Err(not_enough());
    }
    // Go's sort.Float64s: NaN first.
    crate::gosort::stable(&mut xs, |a, b| a < b || (a.is_nan() && !b.is_nan()));
    let mid = xs.len() / 2;
    if xs.len() % 2 == 1 {
        return Ok(Value::num(clean(xs[mid])));
    }
    finite((xs[mid - 1] + xs[mid]) / 2.0)
}

/// The sample standard deviation (divides by n-1); at least two values.
fn stdev(args: &[Value]) -> Result<Value> {
    let xs = numbers(args)?;
    if xs.len() < 2 {
        return Err(not_enough());
    }
    let m = total(&xs) / xs.len() as f64;
    let sum = xs.iter().fold(0.0, |s, x| {
        let d = x - m;
        s + d * d
    });
    finite((sum / (xs.len() - 1) as f64).sqrt())
}
