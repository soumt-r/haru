//! A function (or method) made twice in one place, and a class's second
//! constructor, are syntax errors: Hari has no overloading. Overriding a
//! parent's method, a class's own method beside its objects' of the same
//! name, and the same name in different functions are not duplicates.
//! Hana's tests/duplicate_definition_test.go has the same cases.

use haru_core::lang::{HARI, KANADE};
use haru_core::run_to_string;

const HEADING: &str = "구문 분석(Parsing) 중 오류가 발생했어요:\n";

#[test]
fn a_function_made_twice_in_one_place_is_a_syntax_error() {
    let cases = [
        ("<인사>를 만들자 ():\n    1을 돌려주자\n<인사>를 만들자 ('x'):\n    'x'를 돌려주자\n", "3번째 줄 1번째 글자", "<인사>"),
        ("[상자]를 설계하자:\n    <값>을 만들자 ():\n        1을 돌려주자\n    <값>을 만들자 ():\n        2를 돌려주자\n", "4번째 줄 5번째 글자", "<값>"),
        ("<바깥>을 만들자 ():\n    <안>을 만들자 ():\n        1을 돌려주자\n    <안>을 만들자 ():\n        2를 돌려주자\n", "4번째 줄 5번째 글자", "<안>"),
        // A declaration that looks like an assignment.
        ("<세배>를 만들자 ('x'):\n    ('x' * 3)를 돌려주자\n'보관'을 <세배>로 정하자\n", "3번째 줄 7번째 글자", "<세배>"),
    ];
    for (src, place, name) in cases {
        let want = format!("{HEADING}  - SyntaxError: {place}: 같은 곳에 이름이 {name}인 함수가 이미 있어요. 함수는 이름마다 하나만 만들 수 있어요.\n");
        assert_eq!(run_to_string(src, &HARI), want, "{src}");
    }
}

#[test]
fn a_second_constructor_is_a_syntax_error() {
    let src = "[점]을 설계하자:\n    처음 만들어질 때 () 다음과 같이 하자:\n        1을 출력하자\n    처음 만들어질 때 ('x') 다음과 같이 하자:\n        2를 출력하자\n";
    let want = format!("{HEADING}  - SyntaxError: 4번째 줄 5번째 글자: 이 설계에는 '처음 만들어질 때'가 이미 있어요. 설계마다 하나만 만들 수 있어요.\n");
    assert_eq!(run_to_string(src, &HARI), want);
}

#[test]
fn kanade_reports_duplicates_too() {
    let src = "【箱】を設計しよう:\n    〈値〉を作ろう():\n        1を返そう\n    〈値〉を作ろう():\n        2を返そう\n";
    let want = "構文解析中にエラーが発生しました:\n  - SyntaxError: 4行目、5文字目: 同じところに〈値〉という関数がすでにあります。関数は名前ごとに一つしか作れません。\n";
    assert_eq!(run_to_string(src, &KANADE), want);
}

#[test]
fn overriding_and_other_places_are_not_duplicates() {
    let cases = [
        // overriding in a child class
        "[동물]을 설계하자:\n    <소리>를 만들자 ():\n        \"...\"를 돌려주자\n[개]는 [동물]을 바탕으로 하고 설계하자:\n    <소리>를 만들자 ():\n        \"멍\"을 돌려주자\n(새로운 [개]()의 <소리>())를 출력하자\n",
        // a class's own method and its objects' method of the same name
        "[공장]을 설계하자:\n    <만들기>를 만들자 ():\n        1을 돌려주자\n    '우리'의 <만들기>를 만들자 ():\n        2를 돌려주자\n\"멍\"을 출력하자\n",
        // the same name in two functions' bodies
        "<가>를 만들자 ():\n    <도움>을 만들자 ():\n        1을 돌려주자\n<나>를 만들자 ():\n    <도움>을 만들자 ():\n        2를 돌려주자\n\"멍\"을 출력하자\n",
    ];
    for src in cases {
        assert_eq!(run_to_string(src, &HARI), "멍\n", "{src}");
    }
}
