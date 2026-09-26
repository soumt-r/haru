//! The cycle collector frees what only cycles hold, and nothing else.

use haru_core::lang::HARI;
use haru_core::{gc, run_to_string};

#[test]
fn cycles_the_program_still_reaches_survive_collections() {
    // A ring of three objects and a list inside a dictionary inside itself,
    // kept in variables, while 60,000 garbage cycles force collections.
    let src = "\
[노드]를 설계하자:
    '이름'을 \"\"로 정하자
    '다음'을 비어있음으로 정하자
'가'를 새로운 [노드]()로 정하자
'나'를 새로운 [노드]()로 정하자
'다'를 새로운 [노드]()로 정하자
'가'의 '이름'을 \"가\"로 정하자
'나'의 '이름'을 \"나\"로 정하자
'다'의 '이름'을 \"다\"로 정하자
'가'의 '다음'을 '나'로 정하자
'나'의 '다음'을 '다'로 정하자
'다'의 '다음'을 '가'로 정하자
'사전'을 {\"목록\": [1, 2]}로 정하자
('사전'의 \"목록\")에 '사전'을 추가하자
'나'를 비어있음으로 정하자
'다'를 비어있음으로 정하자
1부터 20000까지 반복하자 ('i'):
    'a'를 [1]로 정하자
    'a'에 'a'를 추가하자
    'x'를 새로운 [노드]()로 정하자
    'x'의 '다음'을 'x'로 정하자
    'd'를 {}로 정하자
    'd'의 \"나\"를 'd'로 정하자
'지금'을 '가'로 정하자
1부터 4까지 반복하자 ('i'):
    ('지금'의 '이름')을 이어출력하자
    '지금'을 '지금'의 '다음'으로 정하자
\"\"를 출력하자
'안'을 ('사전'의 \"목록\")의 3번째로 정하자
('안'의 \"목록\")의 2번째를 출력하자
";
    let before = gc::stats();
    assert_eq!(run_to_string(src, &HARI), "가나다가\n2\n");
    let (runs, freed) = gc::stats();
    assert!(runs > before.0, "no collection ran");
    assert!(freed - before.1 >= 50_000, "freed only {}", freed - before.1);
}
