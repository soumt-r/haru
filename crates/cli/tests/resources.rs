//! Resources: native objects the program holds as values with methods
//! (the example package's counter), freed when the program lets go of them.

use haru_cli::packages::{std_runtime, Resolver};
use haru_core::compiler::compile;
use haru_core::lang::HARI;
use haru_core::run_compiled_to_string;

fn run(src: &str) -> String {
    let mut rt = std_runtime();
    rt.load_static(greet::haru_entry).unwrap();
    let resolver = Resolver::new(rt, Vec::new(), &[]);
    let (program, diags) = haru_syntax::parse(src, HARI.syntax);
    assert!(diags.is_empty(), "{diags:?}");
    let compiled = compile(&program, &HARI, &resolver).unwrap();
    let rt = resolver.rt.borrow();
    run_compiled_to_string(&compiled, &HARI, &rt)
}

#[test]
fn a_resource_has_methods_and_is_freed_when_let_go() {
    let out = run("\
[인사]에서 <계수기>와 <해제된수>를 가져오자
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
");
    assert_eq!(out, "15\n15\n[계수기 객체]\n0\n1\n");
}

#[test]
fn a_resource_inside_a_cycle_is_freed_by_the_collector() {
    let out = run("\
[인사]에서 <계수기>와 <해제된수>를 가져오자
'시작'을 <해제된수>()로 정하자
'l'을 [<계수기>(1)]로 정하자
'l'에 'l'을 추가하자
'l'을 비어있음으로 정하자
(<해제된수>() - '시작')를 출력하자
1부터 30000까지 반복하자 ('i'):
    'x'를 [1]로 정하자
(<해제된수>() - '시작')를 출력하자
");
    assert_eq!(out, "0\n1\n");
}

#[test]
fn a_resources_errors() {
    let out = run("\
[인사]에서 <계수기>를 가져오자
'c'를 <계수기>(1)로 정하자
일단 해보자:
    'c'의 <없는것>()을 실행하자
오류가 발생했다면 ('e'):
    ('e'의 '메시지')를 출력하자
일단 해보자:
    'c'의 <더하기>(\"하나\")를 실행하자
오류가 발생했다면 ('e'):
    ('e'의 '메시지')를 출력하자
");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert!(lines[0].contains("없는것"), "{out}");
    assert!(lines[1].contains("숫자"), "{out}");
}
