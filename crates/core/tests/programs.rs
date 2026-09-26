//! Programs with the output Hana gives them (checked against `hana run`).
//! The broad comparison is tools/runcheck.sh; these keep known behaviour,
//! Hana's quirks included, from regressing without Hana at hand.

use haru_core::lang::{HARI, KANADE};
use haru_core::run_to_string;

fn hari(src: &str) -> String {
    run_to_string(src, &HARI)
}

/// Spec 4.4: a break with no loop of its own function around it is an
/// error where it runs; it does not end the caller's loop.
#[test]
fn break_in_a_function_is_an_error_there() {
    let src = "<탈출>을 만들자 ():\n    반복을 끝내자\n1부터 5까지 반복하자 ('i'):\n    'i'를 출력하자\n    <탈출>()을 실행하자\n\"루프 뒤\"를 출력하자\n";
    assert_eq!(hari(src), "1\n런타임 오류: IllegalBreakError: '반복을 끝내자'는 반복 안에서만 쓸 수 있어요.\n");
}

#[test]
fn a_function_declared_in_a_block_is_never_found() {
    let src = "만약 참 라면:\n    <안쪽>을 만들자 ():\n        \"안쪽\"을 출력하자\n<안쪽>()을 실행하자\n";
    assert_eq!(hari(src), "런타임 오류: MethodNotFoundError: '안쪽' 함수를 찾을 수 없어요.\n");
}

#[test]
fn top_level_break_is_an_error_and_return_ends_the_program() {
    assert_eq!(hari("\"전\"을 출력하자\n반복을 끝내자\n"), "전\n런타임 오류: IllegalBreakError: '반복을 끝내자'는 반복 안에서만 쓸 수 있어요.\n");
    assert_eq!(hari("\"전\"을 출력하자\n1을 돌려주자\n\"후\"를 출력하자\n"), "전\n");
    let finally = "일단 해보자:\n    2를 돌려주자\n마무리는 항상:\n    \"마무리\"를 출력하자\n\"뒤\"를 출력하자\n";
    assert_eq!(hari(finally), "마무리\n");
}

#[test]
fn declared_types_stick() {
    let src = "'x'를 [숫자]인 1로 정하자\n'x'를 \"a\"로 정하자\n";
    assert_eq!(hari(src), "런타임 오류: TypeError: 'x'에는 '숫자' 타입만 담을 수 있어요. '문자열' 값이 들어왔어요.\n");
}

#[test]
fn values_print_like_hana() {
    let src = r#"'목록'을 [1, 2]로 정하자
'목록'의 5번째를 9로 정하자
'목록'을 출력하자
'사전'을 {"b": 1, "a": 2, 3: 참}로 정하자
'사전'을 출력하자
(10 % 3)을 출력하자
(7 / 2)을 출력하자
(0.1 + 0.2)를 출력하자
(1000000 * 1000000 * 1000000 * 1000)을 출력하자
<문자로>(3.5)를 출력하자
틀"값: {'목록'} {1 + 2}"를 출력하자
(0 * -1)을 출력하자
'목록'의 '없는변수'를 출력하자
"#;
    assert_eq!(
        hari(src),
        "[1, 2]\n{3: 참, a: 2, b: 1}\n1\n3.5\n0.30000000000000004\n1000000000000000000000\n3.5\n값: [1, 2] 3\n0\n런타임 오류: TypeError: 목록의 위치(인덱스)는 숫자여야 해요.\n"
    );
}

#[test]
fn a_loop_pass_is_a_new_scope() {
    let src = "1부터 2까지 반복하자 ('i'):\n    만약 ('i' == 2) 라면:\n        '안'을 출력하자\n    '안'을 'i'로 정하자\n";
    assert_eq!(hari(src), "런타임 오류: ReferenceError: '안' 변수를 찾을 수 없어요.\n");
}

#[test]
fn functions_see_globals_but_not_callers() {
    let src = "<보기>를 만들자 ():\n    '전역'을 출력하자\n    '지역'을 출력하자\n'전역'을 1로 정하자\n1부터 1까지 반복하자:\n    '지역'을 2로 정하자\n    <보기>()를 실행하자\n";
    assert_eq!(hari(src), "1\n런타임 오류: ReferenceError: '지역' 변수를 찾을 수 없어요.\n");
}

#[test]
fn deep_recursion_is_a_catchable_limit() {
    let src = "<내려가기>를 만들자 ('n'):\n    <내려가기>(('n' + 1))를 돌려주자\n<내려가기>(1)을 출력하자\n";
    assert_eq!(hari(src), "런타임 오류: RecursionError: 호출이 너무 깊게 쌓여서 10000단계를 넘었어요.\n");
}

#[test]
fn kanade_prints_in_its_own_words() {
    let src = "『x』を【数字】の1にしよう\n『x』を出力しよう\n空っぽを出力しよう\n『y』を出力しよう\n";
    assert_eq!(run_to_string(src, &KANADE), "1\n空っぽ\nランタイムエラー: ReferenceError: 変数『y』が見つかりません。\n");
}

#[test]
fn field_initializers_run_in_the_callers_scope_and_parents_fields_are_not_made() {
    let src = "[부모]를 설계하자:\n    '부모필드'를 1로 정하자\n[자식]은 [부모]를 바탕으로 하고 설계하자:\n    '필드'를 '바깥값'으로 정하자\n    <보기>를 만들자 ():\n        '필드'를 출력하자\n        '부모필드'를 출력하자\n1부터 1까지 반복하자:\n    '바깥값'을 42로 정하자\n    '객체'를 새로운 [자식]()로 정하자\n    '객체'의 <보기>()를 실행하자\n";
    assert_eq!(hari(src), "42\n런타임 오류: ReferenceError: '부모필드' 변수를 찾을 수 없어요.\n");
}

#[test]
fn what_a_handler_catches() {
    let src = r#"일단 해보자:
    [오류]("직접")를 발생시키자
오류가 발생했다면 ('e'):
    'e'의 '메시지'를 출력하자
일단 해보자:
    "글자"를 발생시키자
오류가 발생했다면 ('e'):
    'e'의 '메시지'를 출력하자
    'e'를 출력하자
일단 해보자:
    3.5를 발생시키자
오류가 발생했다면 ('e'):
    'e'의 '메시지'를 출력하자
1부터 3까지 반복하자 ('i'):
    일단 해보자:
        만약 ('i' == 2) 라면:
            반복을 끝내자
        'i'를 출력하자
    마무리는 항상:
        "마무리"를 출력하자
일단 해보자:
    (1 / 0)을 출력하자
오류가 발생했다면 ('e'):
    'e'의 '메시지'를 출력하자
새로운 [오류]("밖")을 발생시키자
"#;
    assert_eq!(
        hari(src),
        "TypeError: 호출할 수 없는 값이에요.\n글자\n[오류 객체]\n3.5\n1\n마무리\n마무리\nDivideByZeroError: 0으로 나눌 수 없어요.\n런타임 오류: 밖\n"
    );
}
