//! A package in Hari and Kanade source only (examples/greet_source): the
//! same greet package, found by `haru run` in a project's packages folder.

use std::path::Path;
use std::process::Command;

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn run(file: &str, program: &str) -> String {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("source_package_{file}"));
    let _ = std::fs::remove_dir_all(&dir);
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/greet_source");
    copy_dir(&example, &dir.join("packages").join("greet_source"));
    std::fs::write(dir.join(file), program).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_haru")).args(["run", file]).current_dir(&dir).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

#[test]
fn the_hari_entry_point() {
    let out = run(
        "main.hr",
        "\
[greet_source]에서 전부 가져오자
<인사말>(\"하리\")를 출력하자
'목'을 [1]로 정하자
<두번추가>('목', 5)를 실행하자
'목'을 출력하자
(<합계>('목'))을 출력하자
일단 해보자:
    (<합계>([1, \"둘\"]))을 출력하자
오류가 발생했다면 ('e'):
    ('e'의 '메시지')를 출력하자
<두배>를 만들자 ('x'):
    ('x' * 2)를 돌려주자
(<두번적용>(<두배>, 3))을 출력하자
'c'를 새로운 [계수기](10)로 정하자
('c'의 <더하기>(5))를 출력하자
('c'의 <값>())를 출력하자
",
    );
    assert_eq!(out, "안녕, 하리!\n[1, 5, 5]\n11\n2번째 원소가 숫자가 아니에요.\n12\n15\n15\n");
}

#[test]
fn the_kanade_entry_point() {
    let out = run(
        "main.knd",
        "\
【greet_source】から全部持ってこよう
〈挨拶文〉(「ハリ」)を出力しよう
『目』を【1】にしよう
〈二回追加〉(『目』, 5)を実行しよう
『目』を出力しよう
(〈合計〉(『目』))を出力しよう
とりあえずやってみよう:
    (〈合計〉(【1, 「二」】))を出力しよう
発生したら(『e』):
    (『e』の『メッセージ』)を出力しよう
〈倍〉を作ろう(『x』):
    (『x』 * 2)を返そう
(〈二回適用〉(〈倍〉, 3))を出力しよう
『c』を新しい【カウンター】(10)にしよう
(『c』の〈足す〉(5))を出力しよう
(『c』の〈値〉())を出力しよう
",
    );
    assert_eq!(out, "こんにちは、ハリ！\n[1, 5, 5]\n11\n2番目の要素が数ではありません。\n12\n15\n15\n");
}
