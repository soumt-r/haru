//! Packages as `haru run` finds them: the bundled timezone package, a
//! project's own `packages/<이름>` folder, and the errors on the way.
//! Expected outputs are Hana's (checked by hand against `hana run`).

use std::path::{Path, PathBuf};
use std::process::Command;

/// A fresh folder under the target directory.
fn folder(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(dir: &Path, file: &str, text: &str) {
    let path = dir.join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// (standard output, standard error) of `haru run <file>` in `dir`.
fn run(dir: &Path, file: &str) -> (String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_haru"))
        .args(["run", file])
        .current_dir(dir)
        .env_remove("HARU_PACKAGES")
        .output()
        .unwrap();
    (String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"), String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"))
}

#[test]
fn the_timezone_package_comes_with_haru() {
    let dir = folder("pkg_timezone");
    write(
        &dir,
        "main.hr",
        "[timezone]에서 <시간대오프셋>과 <시간대서식>을 가져오자\n\
         <시간대오프셋>(0, \"Asia/Seoul\")을 출력하자\n\
         <시간대서식>(1000000000, \"YYYY-MM-DD HH:mm\", \"America/New_York\")을 출력하자\n\
         <네이티브_Offset>(0, \"+09:30\")을 출력하자\n",
    );
    // The package's native functions come along with the import, as in Hana.
    assert_eq!(run(&dir, "main.hr").0, "32400\n2001-09-08 21:46\n34200\n");

    write(&dir, "bad.hr", "[timezone]에서 <시간대오프셋>을 가져오자\n<시간대오프셋>(0, \"Asia/Nowhere\")을 출력하자\n");
    assert_eq!(
        run(&dir, "bad.hr").1,
        "런타임 오류: ImportError: 'unknown time zone \"Asia/Nowhere\"' 때문에 네이티브 함수 'Offset'이(가) 실패했어요.\n"
    );
}

#[test]
fn a_projects_own_package_folder() {
    let dir = folder("pkg_local");
    write(&dir, "packages/greeter/haru.toml", "[package]\nname = \"greeter\"\n\n[entry]\nhari = \"src/main.hr\"\n");
    write(&dir, "packages/greeter/src/main.hr", "<인사>를 만들자 ():\n    \"안녕\"을 출력하자\n");
    write(&dir, "packages/greeter/hari/index.hr", "<인사>를 만들자 ():\n    \"틀렸어요\"를 출력하자\n");
    write(&dir, "main.hr", "[greeter]에서 <인사>를 가져오자\n<인사>()를 실행하자\n");
    assert_eq!(run(&dir, "main.hr").0, "안녕\n");

    // An entry point for Hari only.
    write(&dir, "main.knd", "【greeter】から〈挨拶〉を持ってこよう\n");
    assert_eq!(
        run(&dir, "main.knd").1,
        "ランタイムエラー: ImportError: パッケージ『greeter』には『kanade』用のエントリがないため、持ってこられません。\n"
    );

    write(&dir, "packages/broken/haru.toml", "[package\n");
    write(&dir, "broken.hr", "[broken]에서 <무엇>을 가져오자\n");
    assert!(run(&dir, "broken.hr").1.contains("ImportError"));
}

#[test]
fn a_git_package_the_project_has_not_installed() {
    let dir = folder("pkg_missing");
    write(&dir, "main.hr", "[github.com/owner/repo]에서 <무엇>을 가져오자\n");
    assert_eq!(
        run(&dir, "main.hr").1,
        "런타임 오류: ImportError: 패키지 '[github.com/owner/repo]'가 설치되어 있지 않아요. 'haru install'로 내려받을 수 있어요.\n"
    );
}
