//! A package written in C (examples/greet_c): the same module as the Rust
//! greet example, through haru.h only, loaded as a shared library, used from
//! a program, and found by `haru run` in a project's packages folder.

use std::path::Path;
use std::process::Command;

use haru_cli::packages::{std_runtime, Resolver};
use haru_core::compiler::compile;
use haru_core::lang::HARI;
use haru_core::{run_compiled_to_string, FnRef, Runtime, Value};

fn runtime() -> Runtime {
    let mut rt = std_runtime();
    rt.load_dynamic(Path::new(haru_example_greet_c::LIBRARY), "greet_c").unwrap();
    rt
}

fn func(rt: &Runtime, lang: &str, module: &str, name: &str) -> FnRef {
    let m = rt.module(lang, module).unwrap_or_else(|| panic!("no module {module}"));
    rt.function(m, lang, name).unwrap_or_else(|| panic!("no function {name}"))
}

#[test]
fn the_c_module_does_what_the_rust_one_does() {
    let rt = runtime();
    let hello = func(&rt, "hari", "C인사", "인사말");
    assert_eq!(rt.call(hello, &[Value::str("하리")]).unwrap(), Value::str("안녕, 하리!"));
    assert_eq!(rt.call(hello, &[Value::str("")]).unwrap(), Value::str("안녕, !"));

    // The host checks the arguments against the descriptor.
    let err = rt.call(hello, &[Value::num(3.0)]).unwrap_err();
    assert_eq!(err.message(&rt, "hari"), "1번째 인자는 [문자열]이어야 해요.");

    // A list goes by reference: the module's pushes are visible here.
    let list = Value::list(vec![Value::num(1.0)]);
    let push_twice = func(&rt, "hari", "C인사", "두번추가");
    rt.call(push_twice, &[list.clone(), Value::num(5.0)]).unwrap();
    assert_eq!(list.to_string(), "[1, 5, 5]");
    let words = Value::list(Vec::new());
    rt.call(push_twice, &[words.clone(), Value::str("글")]).unwrap();
    assert_eq!(words.to_string(), "[글, 글]");

    // The module's own error, in the program's language.
    let sum = func(&rt, "kanade", "C挨拶", "合計");
    assert_eq!(rt.call(sum, std::slice::from_ref(&list)).unwrap(), Value::num(11.0));
    let mixed = Value::list(vec![Value::num(1.0), Value::str("둘")]);
    let err = rt.call(sum, &[mixed]).unwrap_err();
    assert_eq!(err.message(&rt, "hari"), "2번째 원소가 숫자가 아니에요.");
    assert_eq!(err.message(&rt, "kanade"), "2番目の要素が数ではありません。");

    // A callback, and an error inside it passing through the C code.
    let math = rt.module("hari", "수학").unwrap();
    let factorial = rt.function_value(rt.function(math, "hari", "팩토리얼").unwrap());
    let apply_twice = func(&rt, "hari", "C인사", "두번적용");
    assert_eq!(rt.call(apply_twice, &[factorial.clone(), Value::num(3.0)]).unwrap(), Value::num(720.0));
    let err = rt.call(apply_twice, &[factorial, Value::num(-1.0)]).unwrap_err();
    assert_eq!(err.qualified_code(&rt), "ValueError.MathDomain");
}

#[test]
fn the_c_modules_resource_has_methods_and_is_freed() {
    let resolver = Resolver::new(runtime(), Vec::new(), &[]);
    let src = "\
[C인사]에서 <계수기>와 <해제된수>를 가져오자
'c'를 <계수기>(10)로 정하자
('c'의 <더하기>(5))를 출력하자
('c'의 <값>())를 출력하자
'c'를 출력하자
'd'를 'c'로 정하자
'시작'을 <해제된수>()로 정하자
'c'를 비어있음으로 정하자
(<해제된수>() - '시작')를 출력하자
'd'를 비어있음으로 정하자
(<해제된수>() - '시작')를 출력하자
";
    let (program, diags) = haru_syntax::parse(src, HARI.syntax);
    assert!(diags.is_empty(), "{diags:?}");
    let compiled = compile(&program, &HARI, &resolver).unwrap();
    let rt = resolver.rt.borrow();
    assert_eq!(run_compiled_to_string(&compiled, &HARI, &rt), "15\n15\n[계수기 객체]\n0\n1\n");
}

/// The package folder as a project has it: haru.toml and the library where
/// it says, found by `haru run` under `packages/`.
#[test]
fn haru_run_finds_the_c_package_in_the_project() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("c_package");
    let _ = std::fs::remove_dir_all(&dir);
    let pkg = dir.join("packages").join("greet_c");
    std::fs::create_dir_all(pkg.join("native")).unwrap();
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/greet_c");
    std::fs::copy(example.join("haru.toml"), pkg.join("haru.toml")).unwrap();
    let library = Path::new(haru_example_greet_c::LIBRARY);
    std::fs::copy(library, pkg.join("native").join(library.file_name().unwrap())).unwrap();
    std::fs::write(
        dir.join("main.hr"),
        "[greet_c]에서 <인사말>과 <합계>를 가져오자\n<인사말>(\"하리\")를 출력하자\n<합계>([1, 2, 3.5])를 출력하자\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_haru")).args(["run", "main.hr"]).current_dir(&dir).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"), "안녕, 하리!\n6.5\n");
}
