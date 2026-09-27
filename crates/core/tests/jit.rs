//! The JIT changes nothing a program shows: each program here prints the
//! same with the JIT on as interpreted, and what Hana prints (`*.out`,
//! from `hana run`). They aim at what compiled code does itself: calls
//! and returns, arguments and their types, the call depth limit, errors
//! leaving a callee, `마무리는 항상`, methods.

#![cfg(feature = "jit")]

use haru_core::lang::HARI;
use haru_core::run_to_string_with;

fn check(name: &str, src: &str, hana: &str) {
    let src = src.replace("\r\n", "\n");
    // Compiled calls use the machine's stack, as `haru run --jit` does.
    let run = |jit| {
        let src = src.clone();
        std::thread::Builder::new()
            .stack_size(1 << 30)
            .spawn(move || run_to_string_with(&src, &HARI, jit))
            .unwrap()
            .join()
            .unwrap()
    };
    let (interpreted, compiled) = (run(false), run(true));
    assert_eq!(compiled, interpreted, "{name}: the JIT changed the output");
    assert_eq!(compiled.trim_end(), hana.replace("\r\n", "\n").trim_end(), "{name}: not what Hana prints");
}

#[test]
fn deep_calls() {
    check("deep_calls", include_str!("jit/deep_calls.hr"), include_str!("jit/deep_calls.out"));
}

/// Inline caches: a site that sees two classes, fields in other places,
/// access rules from inside and outside, getters and setters, a field's
/// type on write, dictionaries, string appends, typed lists.
#[test]
fn objects() {
    check("objects", include_str!("jit/objects.hr"), include_str!("jit/objects.out"));
}

/// Loops of numbers in registers and every way out of them: another type
/// arriving, errors inside `일단 해보자`, `%` edge cases, breaks, booleans,
/// nested loops, typed and constant variables, -0.
#[test]
fn loops() {
    check("loops", include_str!("jit/loops.hr"), include_str!("jit/loops.out"));
}

/// Lists and dictionaries read and written by compiled code: keys in and
/// out of range, fractions, other types, missing keys, -0, writes past the
/// end, lengths, lists in lists.
#[test]
fn collections() {
    check("collections", include_str!("jit/collections.hr"), include_str!("jit/collections.out"));
}

#[test]
fn calls_and_errors() {
    check("calls_and_errors", include_str!("jit/calls_and_errors.hr"), include_str!("jit/calls_and_errors.out"));
}

/// Calls compiled code makes without frames: an error several calls deep
/// (caught outside, or by a caller in the middle), methods and other
/// helpers inside them, the depth limit, loops of such calls.
#[test]
fn lazy_frames() {
    check("lazy_frames", include_str!("jit/lazy_frames.hr"), include_str!("jit/lazy_frames.out"));
}

/// A method's variables: fields read and declared as variables, a field
/// added later, a global of the same name, a name in two scopes, typed and
/// constant declarations, appending.
#[test]
fn method_vars() {
    check("method_vars", include_str!("jit/method_vars.hr"), include_str!("jit/method_vars.out"));
}

/// Method calls straight into compiled code: a method both classes share
/// and one a subclass overrides, recursion, an error, a private method,
/// a static method, a wrong argument type.
#[test]
fn method_calls() {
    check("method_calls", include_str!("jit/method_calls.hr"), include_str!("jit/method_calls.out"));
}

/// Pushes and pops in compiled code: past the list's room, into a list of
/// numbers, into a constant, from an empty list, from the front, through a
/// variable that holds an object's list.
#[test]
fn lists() {
    check("lists", include_str!("jit/lists.hr"), include_str!("jit/lists.out"));
}
