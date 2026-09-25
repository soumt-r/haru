//! Native modules through the ABI: the standard library (static), the example
//! package linked in (static) and the same package as a dynamic library.

use std::path::PathBuf;

use haru_core::{library_file_name, FnRef, Runtime, Value};

fn std_runtime() -> Runtime {
    let mut rt = Runtime::new();
    for entry in haru_std::MODULES {
        rt.load_static(*entry).unwrap();
    }
    rt
}

fn func(rt: &Runtime, lang: &str, module: &str, name: &str) -> FnRef {
    let m = rt.module(lang, module).unwrap_or_else(|| panic!("no module {module}"));
    rt.function(m, lang, name).unwrap_or_else(|| panic!("no function {name}"))
}

#[test]
fn std_math_is_found_by_name_in_each_language() {
    let rt = std_runtime();
    let ceil = func(&rt, "hari", "수학", "올림");
    assert_eq!(func(&rt, "kanade", "数学", "切り上げ"), ceil);
    assert_eq!(rt.call(ceil, &[Value::num(3.2)]).unwrap(), Value::num(4.0));

    let pow = func(&rt, "hari", "수학", "거듭제곱");
    assert_eq!(rt.call(pow, &[Value::num(2.0), Value::num(10.0)]).unwrap(), Value::num(1024.0));

    let pi = func(&rt, "kanade", "数学", "円周率");
    assert_eq!(rt.call(pi, &[]).unwrap(), Value::num(std::f64::consts::PI));
}

#[test]
fn standard_errors_are_hanas_in_the_program_language() {
    let rt = std_runtime();
    let sqrt = func(&rt, "hari", "수학", "제곱근");
    let err = rt.call(sqrt, &[Value::num(-4.0)]).unwrap_err();
    assert_eq!(err.qualified_code(&rt), "ValueError.MathDomain");
    assert_eq!(err.message(&rt, "hari"), "ValueError: 이 값으로는 계산할 수 없어요.");
    let ceil = func(&rt, "hari", "수학", "올림");
    let err = rt.call(ceil, &[]).unwrap_err();
    assert_eq!(err.message(&rt, "hari"), "ArgumentError: 인자가 1개 필요해요.");
}

#[test]
fn the_host_checks_typed_functions_arguments() {
    let mut rt = std_runtime();
    rt.load_static(greet::haru_entry).unwrap();
    let hello = func(&rt, "hari", "인사", "인사말");

    let err = rt.call(hello, &[]).unwrap_err();
    assert_eq!(err.message(&rt, "hari"), "인자가 1개 필요한데 0개가 들어왔어요.");

    let err = rt.call(hello, &[Value::num(3.0)]).unwrap_err();
    assert_eq!(err.message(&rt, "hari"), "1번째 인자는 [문자열]이어야 해요.");
    assert_eq!(err.message(&rt, "kanade"), "1番目の引数は【文字列】でなければなりません。");
}

fn check_greet(rt: &Runtime) {
    let hello = func(rt, "hari", "인사", "인사말");
    assert_eq!(rt.call(hello, &[Value::str("하리")]).unwrap(), Value::str("안녕, 하리!"));

    // A list goes by reference: the module's pushes are visible here.
    let list = Value::list(vec![Value::num(1.0)]);
    let push_twice = func(rt, "hari", "인사", "두번추가");
    rt.call(push_twice, &[list.clone(), Value::num(5.0)]).unwrap();
    assert_eq!(list.to_string(), "[1, 5, 5]");

    let sum = func(rt, "kanade", "挨拶", "合計");
    assert_eq!(rt.call(sum, std::slice::from_ref(&list)).unwrap(), Value::num(11.0));
    let mixed = Value::list(vec![Value::num(1.0), Value::str("둘")]);
    let err = rt.call(sum, &[mixed]).unwrap_err();
    assert_eq!(err.message(rt, "hari"), "2번째 원소가 숫자가 아니에요.");

    // A callback: the module calls a function value it was given.
    let math = rt.module("hari", "수학").unwrap();
    let ceil = rt.function_value(rt.function(math, "hari", "올림").unwrap());
    let factorial = rt.function_value(rt.function(math, "hari", "팩토리얼").unwrap());
    let apply_twice = func(rt, "hari", "인사", "두번적용");
    assert_eq!(rt.call(apply_twice, &[factorial.clone(), Value::num(3.0)]).unwrap(), Value::num(720.0));
    assert_eq!(rt.call(apply_twice, &[ceil, Value::num(1.5)]).unwrap(), Value::num(2.0));

    // An error inside the callback passes through the module unchanged.
    let err = rt.call(apply_twice, &[factorial, Value::num(-1.0)]).unwrap_err();
    assert_eq!(err.qualified_code(rt), "ValueError.MathDomain");
}

#[test]
fn a_package_linked_into_the_binary() {
    let mut rt = std_runtime();
    rt.load_static(greet::haru_entry).unwrap();
    check_greet(&rt);
}

/// The same crate as a `.dll`/`.so`/`.dylib`. Cargo builds the cdylib next to
/// the test's dependencies.
#[test]
fn the_same_package_as_a_dynamic_library() {
    let deps = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
    let file = library_file_name("greet");
    let path: PathBuf = [deps.join(&file), deps.parent().unwrap().join(&file)]
        .into_iter()
        .find(|p| p.exists())
        .unwrap_or_else(|| panic!("{file} not built; run `cargo build -p haru-example-greet`"));

    let mut rt = std_runtime();
    rt.load_dynamic(&path, "greet").unwrap();
    check_greet(&rt);
}
