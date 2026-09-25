//! [목록] / 【リスト】, as Hana implements it (`std/stdimpl/list.go` and
//! `hostfuncs.go`). The functions that take a function of the program call it
//! back through the host.

use std::collections::HashSet;

use haru_sdk::abi::tag;
use haru_sdk::prelude::*;
use haru_sdk::IntoRet;

use crate::hana::{between, clean, exactly, integer, list, new_list, MAX_RESULT};

haru_sdk::entry!(pub(crate) fn entry = "list", build);

fn build(m: &mut Module) {
    crate::describe(m, "list", &[
        ("list.sort", sort),
        ("list.reverse", reverse),
        ("list.unique", unique),
        ("list.range", range),
        ("list.flatten", flatten),
        ("list.chunk", chunk),
        ("list.zip", zip),
        ("list.map", map),
        ("list.filter", filter),
        ("list.reduce", reduce),
        ("list.find", find),
        ("list.any", any),
        ("list.all", all),
        ("list.sortby", sort_by),
    ]);
}

fn items(args: &[Value], n: usize) -> Result<Vec<Value>> {
    exactly(args, n)?;
    Ok(list(args, 0)?.iter().collect())
}

/// Go's `sort.Float64s` order: NaN first.
fn float_less(a: f64, b: f64) -> bool {
    a < b || (a.is_nan() && !b.is_nan())
}

/// A sorted copy: all numbers (ascending) or all strings (by code point).
fn sort(args: &[Value]) -> Result<Value> {
    let out = items(args, 1)?;
    let Some(first) = out.first() else { return new_list([]) };
    let not_sortable = || Error::new("TypeError.ListNotSortable");
    match first.tag() {
        tag::NUM => {
            let mut nums = out.iter().map(|v| v.as_num().ok_or_else(not_sortable)).collect::<Result<Vec<f64>>>()?;
            nums.sort_by(|a, b| {
                if float_less(*a, *b) {
                    std::cmp::Ordering::Less
                } else if float_less(*b, *a) {
                    std::cmp::Ordering::Greater
                } else {
                    std::cmp::Ordering::Equal
                }
            });
            new_list(nums.into_iter().map(|f| Value::num(clean(f))))
        }
        tag::STR => {
            let mut strs = out.iter().map(|v| v.as_str().ok_or_else(not_sortable)).collect::<Result<Vec<Str>>>()?;
            strs.sort_by(|a, b| (**a).cmp(&**b));
            new_list(strs.into_iter().map(|s| s.into_ret()).collect::<Result<Vec<Value>>>()?)
        }
        _ => Err(not_sortable()),
    }
}

fn reverse(args: &[Value]) -> Result<Value> {
    let out = items(args, 1)?;
    new_list(out.into_iter().rev())
}

/// What Go's map sees as the same key.
#[derive(PartialEq, Eq, Hash)]
enum Key {
    Null,
    Bool(bool),
    Num(u64),
    Str(String),
}

/// The first of each equal value, in order: only numbers, strings, booleans
/// and 비어있음 can be compared.
fn unique(args: &[Value]) -> Result<Value> {
    let list = items(args, 1)?;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for v in list {
        let key = match v.tag() {
            tag::NULL => Key::Null,
            tag::BOOL => Key::Bool(v.as_bool().unwrap()),
            tag::NUM => {
                let n = v.as_num().unwrap();
                if n.is_nan() {
                    // NaN is never equal to itself: every one stays.
                    out.push(v);
                    continue;
                }
                Key::Num(if n == 0.0 { 0 } else { n.to_bits() })
            }
            tag::STR => Key::Str(v.as_str().unwrap().to_string()),
            _ => return Err(Error::new("TypeError.ListValueUnsupported")),
        };
        if seen.insert(key) {
            out.push(v);
        }
    }
    new_list(out)
}

/// The whole numbers from start to end, both included; the step defaults to
/// 1 (or -1 counting down), and one pointing away from end gives nothing.
fn range(args: &[Value]) -> Result<Value> {
    between(args, 2, 3)?;
    let start = integer(args, 0)?;
    let end = integer(args, 1)?;
    let mut step = if start > end { -1 } else { 1 };
    if args.len() == 3 {
        step = integer(args, 2)?;
        if step == 0 {
            return Err(Error::new("ValueError.RangeStepZero"));
        }
    }
    if (step > 0 && start > end) || (step < 0 && start < end) {
        return new_list([]);
    }
    let count = (end - start) / step + 1;
    if count > MAX_RESULT {
        return Err(Error::new("ValueError.ResultTooLarge"));
    }
    new_list((0..count).map(|n| Value::num((start + n * step) as f64)))
}

/// Opens one level of nested lists.
fn flatten(args: &[Value]) -> Result<Value> {
    let list = items(args, 1)?;
    let mut out = Vec::new();
    for v in list {
        match v.as_list() {
            Some(inner) => out.extend(inner.iter()),
            None => out.push(v),
        }
    }
    new_list(out)
}

/// Lists of n items (the last may be shorter).
fn chunk(args: &[Value]) -> Result<Value> {
    let list = items(args, 2)?;
    let n = integer(args, 1)?;
    if n < 1 {
        return Err(Error::new("TypeError.NativeArgInteger").arg(2.0));
    }
    let pieces = list.chunks(n as usize).map(|c| new_list(c.iter().cloned()));
    new_list(pieces.collect::<Result<Vec<Value>>>()?)
}

/// Pairs of items at the same place, up to the shorter list.
fn zip(args: &[Value]) -> Result<Value> {
    let a = items(args, 2)?;
    let b: Vec<Value> = list(args, 1)?.iter().collect();
    let pairs = a.into_iter().zip(b).map(|(x, y)| new_list([x, y]));
    new_list(pairs.collect::<Result<Vec<Value>>>()?)
}

/// (list, function, ...) for a function that takes n arguments.
fn call_args(args: &[Value], n: usize) -> Result<(Vec<Value>, &Value)> {
    Ok((items(args, n)?, &args[1]))
}

/// Asks the function about one item: the answer has to be true or false.
fn condition(f: &Value, item: &Value) -> Result<bool> {
    f.call(std::slice::from_ref(item))?
        .as_bool()
        .ok_or_else(|| Error::new("TypeError.CallbackNotBoolean"))
}

fn map(args: &[Value]) -> Result<Value> {
    let (list, f) = call_args(args, 2)?;
    let out = list.iter().map(|item| f.call(std::slice::from_ref(item))).collect::<Result<Vec<Value>>>()?;
    new_list(out)
}

fn filter(args: &[Value]) -> Result<Value> {
    let (list, f) = call_args(args, 2)?;
    let mut out = Vec::new();
    for item in list {
        if condition(f, &item)? {
            out.push(item);
        }
    }
    new_list(out)
}

/// Folds the list into one value: f(누적값, 항목), from the third argument.
fn reduce(args: &[Value]) -> Result<Value> {
    let (list, f) = call_args(args, 3)?;
    let mut acc = args[2].clone();
    for item in list {
        acc = f.call(&[acc, item])?;
    }
    Ok(acc)
}

/// The first item the function accepts, or 비어있음.
fn find(args: &[Value]) -> Result<Value> {
    let (list, f) = call_args(args, 2)?;
    for item in list {
        if condition(f, &item)? {
            return Ok(item);
        }
    }
    Ok(Value::NULL)
}

fn any(args: &[Value]) -> Result<Value> {
    let (list, f) = call_args(args, 2)?;
    for item in list {
        if condition(f, &item)? {
            return Ok(Value::bool(true));
        }
    }
    Ok(Value::bool(false))
}

fn all(args: &[Value]) -> Result<Value> {
    let (list, f) = call_args(args, 2)?;
    for item in list {
        if !condition(f, &item)? {
            return Ok(Value::bool(false));
        }
    }
    Ok(Value::bool(true))
}

/// The items ordered by the key the function gives each: all numbers or all
/// strings, ascending; equal keys keep their order.
fn sort_by(args: &[Value]) -> Result<Value> {
    let (list, f) = call_args(args, 2)?;
    let keys = list.iter().map(|item| f.call(std::slice::from_ref(item))).collect::<Result<Vec<Value>>>()?;
    let mut order: Vec<usize> = (0..list.len()).collect();
    if let Some(first) = keys.first() {
        let not_sortable = || Error::new("TypeError.ListNotSortable");
        match first.tag() {
            tag::NUM => {
                let k = keys.iter().map(|v| v.as_num().ok_or_else(not_sortable)).collect::<Result<Vec<f64>>>()?;
                crate::gosort::stable(&mut order, |&x, &y| k[x] < k[y]);
            }
            tag::STR => {
                let k = keys.iter().map(|v| v.as_str().ok_or_else(not_sortable)).collect::<Result<Vec<Str>>>()?;
                crate::gosort::stable(&mut order, |&x, &y| *k[x] < *k[y]);
            }
            _ => return Err(not_sortable()),
        }
    }
    new_list(order.into_iter().map(|i| list[i].clone()))
}
