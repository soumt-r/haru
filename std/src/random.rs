//! [무작위] / 【乱数】, as Hana implements it (`std/stdimpl/random.go`, and
//! the UUID from `encoding.go`). The numbers differ from run to run, as in
//! Hana; the checks and errors are the same.

use haru_sdk::prelude::*;

use crate::hana::{exactly, integer, list, new_list};

haru_sdk::entry!(pub(crate) fn entry = "random", build);

fn build(m: &mut Module) {
    crate::describe(m, "random", &[
        ("random.float", float),
        ("random.int", int),
        ("random.choice", choice),
        ("random.shuffle", shuffle),
        ("random.uuid", uuid),
    ]);
}

/// A number in [0, 1).
fn float(args: &[Value]) -> Result<Value> {
    exactly(args, 0)?;
    Ok(Value::num(fastrand::f64()))
}

/// A whole number between min and max, both included.
fn int(args: &[Value]) -> Result<Value> {
    exactly(args, 2)?;
    let min = integer(args, 0)?;
    let max = integer(args, 1)?;
    if min > max {
        return Err(Error::new("ValueError.RandomRange").arg(min as f64).arg(max as f64));
    }
    Ok(Value::num(fastrand::i64(min..=max) as f64))
}

fn choice(args: &[Value]) -> Result<Value> {
    exactly(args, 1)?;
    let items = list(args, 0)?;
    if items.is_empty() {
        return Err(Error::new("ValueError.RandomEmpty"));
    }
    Ok(items.get(fastrand::usize(..items.len())).unwrap_or(Value::NULL))
}

/// A shuffled copy: the list given is untouched.
fn shuffle(args: &[Value]) -> Result<Value> {
    exactly(args, 1)?;
    let mut items: Vec<Value> = list(args, 0)?.iter().collect();
    fastrand::shuffle(&mut items);
    new_list(items)
}

/// A random (version 4) UUID, from the system's secure source.
fn uuid(args: &[Value]) -> Result<Value> {
    exactly(args, 0)?;
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(|_| Error::new("ValueError.RandomEmpty"))?;
    b[6] = b[6] & 0x0f | 0x40;
    b[8] = b[8] & 0x3f | 0x80;
    let h = crate::encoding::hex(&b);
    Ok(Value::str(&format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])))
}
