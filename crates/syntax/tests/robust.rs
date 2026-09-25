//! The parser never panics, whatever it is given. (Hana's parser panics on
//! some broken input; parity with Hana is checked by `haru ast-check`.)

use haru_syntax::{parse, HARI, KANADE};

const HARI_SAMPLE: &str = r#"[숫자]를 돌려주는 <피보나치>를 만들자 ([숫자]인 'n' = 1):
    만약 ('n' <= 1) 라면:
        'n'을 돌려주자
    그렇지 않고 만약 'n'이 2와 같다면:
        1을 돌려주자
    (<피보나치>('n' - 1) + <피보나치>('n' - 2) * 2)를 돌려주자

[동물]을 바탕으로 하고 [말하는]을 따르는 [강아지]를 설계하자:
    처음 만들어질 때 ('이름') 다음과 같이 하자:
        '나'의 '이름'을 '이름'으로 정하자
    '우리'의 '수'를 0으로 정하자
    '나이'를 [숫자]로 정하자:
        가져올 때:
            1을 돌려주자
        정할 때 ('새값'):
            '새값'을 출력하자

'목록'을 [1, 2, {"a": 3}]로 정하자
'목록' 뒤에 틀"{'x'} 개"를 추가하자
'목록' 앞에서 꺼낸 값을 출력하자
1부터 10까지 반복하자 ('i'):
    반복을 끝내자
'목록'의 '항목'마다 반복하자:
    '항목'을 이어출력하자
'값'에 따라 나누자:
    "a", "b" 인 경우:
        다음으로 이어가자
    나머지는:
        비어있음을 출력하자
일단 해보자:
    [오류]("x")를 발생시키자
[오류]가 발생했다면 ('e'):
    'e'의 '메시지'를 출력하자
마무리는 항상:
    참을 출력하자
[수학]에서 <올림>을 <반올림>으로 가져오자
"lib.hr"에서 전부 가져오자
"#;

const KANADE_SAMPLE: &str = r#"【数字】を返す〈フィボナッチ〉を作ろう(【数字】の『n』):
    もし『n』が1以下ならば:
        『n』を返そう
    もしくは『n』が2と同じなら:
        1を返そう
    それ以外なら:
        (〈フィボナッチ〉(『n』 - 1) + 〈フィボナッチ〉(『n』 - 2))を返そう
『果物』を【「りんご」、「バナナ」】にしよう
『果物』の後ろから取り出した値を出力しよう
『果物』の『x』ごとに繰り返そう:
    『x』を続けて出力しよう
【数学】から全部持ってこよう
"#;

fn survive(sample: &str, profile: &'static haru_syntax::Profile) {
    let chars: Vec<char> = sample.chars().collect();
    for i in 0..=chars.len() {
        let cut: String = chars[..i].iter().collect();
        parse(&cut, profile);
        let without: String = chars[..i].iter().chain(chars.get(i + 1..).unwrap_or(&[])).collect();
        parse(&without, profile);
        let doubled: String = chars[..i].iter().chain(chars[i.saturating_sub(5)..].iter()).collect();
        parse(&doubled, profile);
    }
}

#[test]
fn hari_never_panics() {
    survive(HARI_SAMPLE, &HARI);
}

#[test]
fn kanade_never_panics() {
    survive(KANADE_SAMPLE, &KANADE);
}

#[test]
fn samples_parse_cleanly() {
    assert_eq!(parse(HARI_SAMPLE, &HARI).1, vec![]);
    assert_eq!(parse(KANADE_SAMPLE, &KANADE).1, vec![]);
}
