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

#[test]
fn calls_and_errors() {
    check("calls_and_errors", include_str!("jit/calls_and_errors.hr"), include_str!("jit/calls_and_errors.out"));
}
