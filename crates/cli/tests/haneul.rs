//! The bundled 하늘 (空) web framework: routing, requests and responses,
//! hooks, error handlers, sessions and templates through `<시험요청>` (no
//! network), then `haru run` serving over HTTP.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use haru_cli::packages::{std_runtime, Resolver};
use haru_core::compiler::compile;
use haru_core::lang::{Lang, HARI, KANADE};
use haru_core::run_compiled_to_string;

fn run_in(src: &str, lang: &'static Lang) -> String {
    let resolver = Resolver::new(std_runtime(), Vec::new(), &[]);
    let (program, diags) = haru_syntax::parse(src, lang.syntax);
    assert!(diags.is_empty(), "{diags:?}");
    let compiled = compile(&program, lang, &resolver).unwrap();
    let rt = resolver.rt.borrow();
    run_compiled_to_string(&compiled, lang, &rt)
}

fn run(src: &str) -> String {
    run_in(src, &HARI)
}

/// A program that defines an app `'앱'` and `<보기>(메서드, 경로, 본문)`,
/// which prints the status, the content type and the body of the answer.
fn with_app(body: &str) -> String {
    run(&format!(
        "[하늘]에서 <앱>, <JSON응답>, <리다이렉트>, <중단>, <템플릿문자열>, <응답>을 가져오자\n\
         '앱'을 <앱>()으로 정하자\n\
         <보기>를 만들자 ('메서드', '경로', '본문'):\n    \
             '응답'을 '앱'의 <시험요청>('메서드', '경로', '본문')으로 정하자\n    \
             틀\"{{'응답'의 <상태>()}} {{'응답'의 <헤더>(\"Content-Type\")}} {{'응답'의 <본문>()}}\"을 출력하자\n\
         {body}"
    ))
}

#[test]
fn routes_answer_with_what_handlers_return() {
    let out = with_app(
        "\
<홈>을 만들자 ():
    \"<h1>안녕</h1>\"을 돌려주자
<사용자>를 만들자 ('요청'):
    ({\"이름\": '요청'의 <인자>(\"이름\"), \"q\": '요청'의 <쿼리>(\"q\", \"없음\")})를 돌려주자
<글>을 만들자 ('요청'):
    <템플릿문자열>(\"{{ n + 1 }}\", {\"n\": '요청'의 <인자>(\"번호\")})를 돌려주자
<만들기>를 만들자 ('요청'):
    <JSON응답>({\"받음\": '요청'의 <JSON>()}, 201)을 돌려주자
<아무것도>를 만들자 ():
    'x'를 1로 정하자
'앱'의 <GET>(\"/\", <홈>)을 실행하자
'앱'의 <GET>(\"/사용자/<이름>\", <사용자>)를 실행하자
'앱'의 <GET>(\"/글/<int:번호>\", <글>)을 실행하자
'앱'의 <POST>(\"/글\", <만들기>)를 실행하자
'앱'의 <라우트>([\"PUT\", \"PATCH\"], \"/빈\", <아무것도>)를 실행하자
<보기>(\"GET\", \"/\", 비어있음)를 실행하자
<보기>(\"GET\", \"/사용자/%ED%95%98%EB%A3%A8?q=%EA%B0%80+%EB%82%98\", 비어있음)를 실행하자
<보기>(\"GET\", \"/사용자/하루\", 비어있음)를 실행하자
<보기>(\"GET\", \"/글/41\", 비어있음)를 실행하자
<보기>(\"POST\", \"/글\", {\"제목\": \"첫 글\", \"태그\": [1, 2]})를 실행하자
<보기>(\"PATCH\", \"/빈\", 비어있음)를 실행하자
<보기>(\"HEAD\", \"/\", 비어있음)를 실행하자
",
    );
    assert_eq!(
        out,
        "200 text/html; charset=utf-8 <h1>안녕</h1>\n\
         200 application/json {\"q\":\"가 나\",\"이름\":\"하루\"}\n\
         200 application/json {\"q\":\"없음\",\"이름\":\"하루\"}\n\
         200 text/html; charset=utf-8 42\n\
         201 application/json {\"받음\":{\"제목\":\"첫 글\",\"태그\":[1,2]}}\n\
         204 비어있음 \n\
         200 text/html; charset=utf-8 \n"
    );
}

#[test]
fn http_errors_redirects_and_methods() {
    let out = with_app(
        "\
<글>을 만들자 ('요청'):
    만약 ('요청'의 <인자>(\"번호\") > 100) 라면:
        <중단>(404, \"그런 글은 없어요\")를 실행하자
    \"글\"을 돌려주자
<만들기>를 만들자 ('요청'):
    ('요청'의 <JSON>())을 돌려주자
<옛주소>를 만들자 ():
    <리다이렉트>(\"/새주소\")를 돌려주자
<터짐>을 만들자 ():
    (1 / \"가\")를 돌려주자
<숫자>를 만들자 ():
    3을 돌려주자
'앱'의 <GET>(\"/글/<int:번호>\", <글>)을 실행하자
'앱'의 <POST>(\"/글\", <만들기>)를 실행하자
'앱'의 <GET>(\"/옛날\", <옛주소>)를 실행하자
'앱'의 <GET>(\"/목록/\", <옛주소>)를 실행하자
'앱'의 <GET>(\"/터짐\", <터짐>)을 실행하자
'앱'의 <GET>(\"/숫자\", <숫자>)를 실행하자
'r'을 '앱'의 <시험요청>(\"GET\", \"/글/700\")으로 정하자
('r'의 <상태>())를 출력하자
('r'의 <본문>()의 <포함확인>(\"그런 글은 없어요\"))를 출력하자
('앱'의 <시험요청>(\"GET\", \"/글/abc\")의 <상태>())를 출력하자
('앱'의 <시험요청>(\"POST\", \"/글\", \"{{잘못\")의 <상태>())를 출력하자
'r'을 '앱'의 <시험요청>(\"DELETE\", \"/글\")으로 정하자
틀\"{'r'의 <상태>()} {'r'의 <헤더>(\"Allow\")}\"를 출력하자
'r'을 '앱'의 <시험요청>(\"OPTIONS\", \"/글/1\")으로 정하자
틀\"{'r'의 <상태>()} {'r'의 <헤더>(\"Allow\")}\"를 출력하자
'r'을 '앱'의 <시험요청>(\"GET\", \"/옛날\")으로 정하자
틀\"{'r'의 <상태>()} {'r'의 <헤더>(\"Location\")}\"를 출력하자
'r'을 '앱'의 <시험요청>(\"GET\", \"/목록?a=1\")으로 정하자
틀\"{'r'의 <상태>()} {'r'의 <헤더>(\"Location\")}\"를 출력하자
('앱'의 <시험요청>(\"GET\", \"/터짐\")의 <상태>())를 출력하자
('앱'의 <시험요청>(\"GET\", \"/숫자\")의 <상태>())를 출력하자
",
    );
    assert_eq!(
        out,
        "404\n참\n404\n400\n405 OPTIONS, POST\n200 GET, HEAD, OPTIONS\n302 /새주소\n308 /목록/?a=1\n500\n500\n"
    );
}

#[test]
fn hooks_error_handlers_cookies_and_sessions() {
    let out = with_app(
        "\
'앱'의 <비밀키>(\"비밀\")를 실행하자
<확인>을 만들자 ('요청'):
    만약 ('요청'의 <경로>() == \"/비밀\") 라면:
        만약 ('요청'의 <쿠키>(\"토큰\") != \"열려라\") 라면:
            <중단>(401)을 실행하자
    '요청'의 <넣기>(\"사용자\", \"하루\")를 실행하자
<붙이기>를 만들자 ('요청', '응답'):
    '응답'의 <헤더설정>(\"X-Who\", '요청'의 <꺼내기>(\"사용자\", \"?\"))를 실행하자
<없음>을 만들자 ('요청', '메시지'):
    틀\"없어요: {'요청'의 <경로>()} ({'메시지'})\"를 돌려주자
<세기>를 만들자 ('요청'):
    '방문'을 ('요청'의 <세션값>(\"방문\", 0) + 1)로 정하자
    '요청'의 <세션설정>(\"방문\", '방문')을 실행하자
    틀\"{'방문'}번째\"를 돌려주자
<잊기>를 만들자 ('요청'):
    '요청'의 <세션비우기>()를 실행하자
    \"잊었어요\"를 돌려주자
<비밀>을 만들자 ('요청'):
    틀\"비밀, {'요청'의 <꺼내기>(\"사용자\")}\"를 돌려주자
<쿠키>를 만들자 ():
    <응답>(\"줬어요\")의 <쿠키설정>(\"토큰\", \"열려라\", {\"max_age\": 60, \"httponly\": 참})를 돌려주자
'앱'의 <요청전>(<확인>)을 실행하자
'앱'의 <요청후>(<붙이기>)를 실행하자
'앱'의 <오류처리>(404, <없음>)을 실행하자
'앱'의 <GET>(\"/\", <세기>)를 실행하자
'앱'의 <GET>(\"/잊기\", <잊기>)를 실행하자
'앱'의 <GET>(\"/비밀\", <비밀>)을 실행하자
'앱'의 <GET>(\"/쿠키\", <쿠키>)를 실행하자
<보기>(\"GET\", \"/\", 비어있음)를 실행하자
<보기>(\"GET\", \"/\", 비어있음)를 실행하자
<보기>(\"GET\", \"/잊기\", 비어있음)를 실행하자
<보기>(\"GET\", \"/\", 비어있음)를 실행하자
<보기>(\"GET\", \"/없는곳\", 비어있음)를 실행하자
('앱'의 <시험요청>(\"GET\", \"/비밀\")의 <상태>())를 출력하자
'r'을 '앱'의 <시험요청>(\"GET\", \"/쿠키\")로 정하자
('r'의 <헤더>(\"Set-Cookie\"))를 출력하자
('r'의 <헤더>(\"X-Who\"))를 출력하자
<보기>(\"GET\", \"/비밀\", 비어있음)를 실행하자
'r'을 '앱'의 <시험요청>(\"GET\", \"/\", 비어있음, {\"Cookie\": \"session=eyJ4IjoxfQ.forged\"})로 정하자
('r'의 <본문>())을 출력하자
",
    );
    assert_eq!(
        out,
        "200 text/html; charset=utf-8 1번째\n\
         200 text/html; charset=utf-8 2번째\n\
         200 text/html; charset=utf-8 잊었어요\n\
         200 text/html; charset=utf-8 1번째\n\
         404 text/html; charset=utf-8 없어요: /없는곳 (요청한 주소를 찾을 수 없어요.)\n\
         401\n\
         토큰=%EC%97%B4%EB%A0%A4%EB%9D%BC; Max-Age=60; HttpOnly; Path=/\n\
         하루\n\
         200 text/html; charset=utf-8 비밀, 하루\n\
         1번째\n"
    );
}

#[test]
fn templates_from_a_folder_extend_and_escape() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("haneul_templates");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("base.html"), "<title>{% block title %}하늘{% endblock %}</title>\n{% block body %}{% endblock %}\n").unwrap();
    std::fs::write(
        dir.join("list.html"),
        "{% extends \"base.html\" %}\n{% block title %}{{ 제목 }} | {{ super() }}{% endblock %}\n{% block body %}\n<ul>\n  {% for x in 목록 %}\n  <li>{{ loop.index }}. {{ x.이름 }}{% if x.끝 %} ✔{% endif %}</li>\n  {% endfor %}\n</ul>\n{% endblock %}\n",
    )
    .unwrap();
    let folder = dir.display().to_string().replace('\\', "/");
    let out = run(&format!(
        "[하늘]에서 <앱>, <템플릿>을 가져오자\n\
         '앱'을 <앱>()으로 정하자\n\
         '앱'의 <템플릿폴더>(\"{folder}\")를 실행하자\n\
         <목록>을 만들자 ():\n    \
             <템플릿>(\"list.html\", {{\"제목\": \"할 일\", \"목록\": [{{\"이름\": \"장보기\", \"끝\": 참}}, {{\"이름\": \"<청소>\", \"끝\": 거짓}}]}})를 돌려주자\n\
         <없음>을 만들자 ():\n    \
             <템플릿>(\"없는.html\")을 돌려주자\n\
         '앱'의 <GET>(\"/\", <목록>)을 실행하자\n\
         '앱'의 <GET>(\"/없음\", <없음>)을 실행하자\n\
         '앱'의 <디버그>(참)를 실행하자\n\
         ('앱'의 <시험요청>(\"GET\", \"/\")의 <본문>())을 출력하자\n\
         ('앱'의 <시험요청>(\"GET\", \"/없음\")의 <본문>())을 출력하자\n"
    ));
    assert_eq!(
        out,
        "<title>할 일 | 하늘</title>\n\
         <ul>\n  <li>1. 장보기 ✔</li>\n  <li>2. &lt;청소&gt;</li>\n</ul>\n\
         \n\
         <!doctype html>\n<html lang=\"ko\">\n<title>500 Internal Server Error</title>\n<h1>Internal Server Error</h1>\n\
         <p>서버에서 오류가 났어요.</p>\n<pre>템플릿 오류: 템플릿을 찾을 수 없어요: 없는.html</pre>\n\n"
    );
}

#[test]
fn kanade_has_the_same_framework() {
    let out = run_in(
        "\
【空】から〈アプリ〉、〈テンプレート文字列〉を持ってこよう
『アプリ』を〈アプリ〉()にしよう
〈ホーム〉を作ろう(『リクエスト』):
    枠「こんにちは、{『リクエスト』の〈引数〉(「名前」)}さん」を返そう
〈一覧〉を作ろう():
    〈テンプレート文字列〉(「{% for x in xs %}{{ x }}{% if x is even %}!{% endif %} {% endfor %}{{ ok }}」, {「xs」: 【1, 2, 3】, 「ok」: 真})を返そう
『アプリ』の〈GET〉(「/こんにちは/<名前>」, 〈ホーム〉)を実行しよう
『アプリ』の〈GET〉(「/一覧」, 〈一覧〉)を実行しよう
『アプリ』の〈テストリクエスト〉(「GET」, 「/こんにちは/花子」)の〈本文〉()を出力しよう
『アプリ』の〈テストリクエスト〉(「GET」, 「/一覧」)の〈本文〉()を出力しよう
『アプリ』の〈テストリクエスト〉(「GET」, 「/ない」)の〈本文〉()を出力しよう
",
        &KANADE,
    );
    assert_eq!(
        out,
        "こんにちは、花子さん\n1 2! 3 真\n\
         <!doctype html>\n<html lang=\"ja\">\n<title>404 Not Found</title>\n<h1>Not Found</h1>\n<p>お探しのページが見つかりません。</p>\n\n"
    );
}

#[test]
fn a_variable_does_not_hide_an_imported_function() {
    // Hana keeps imported functions apart from variables: `'응답'` in any
    // function is a variable of its own, and `<응답>` stays the function.
    let out = run(
        "\
[하늘]에서 <응답>을 가져오자
<만들기>를 만들자 ():
    <응답>(\"b\")를 돌려주자
<보기>를 만들자 ():
    '응답'을 <만들기>()로 정하자
    '응답'의 <본문>()을 돌려주자
<보기>()를 출력하자
'응답'을 1로 정하자
(<응답>(\"c\")의 <본문>())을 출력하자
'응답'을 출력하자
",
    );
    assert_eq!(out, "b\nc\n1\n");
}

// ---------------------------------------------------------------------------
// Over the network

const PORT: u16 = 47293;

fn connect() -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match TcpStream::connect(("127.0.0.1", PORT)) {
            Ok(s) => return s,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => panic!("no server: {e}"),
        }
    }
}

/// Reads one answer: its status line, headers (lower-case names) and body.
fn read_answer(r: &mut BufReader<TcpStream>) -> (String, Vec<(String, String)>, String) {
    let mut status = String::new();
    r.read_line(&mut status).unwrap();
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let (k, v) = line.split_once(':').unwrap();
        headers.push((k.to_ascii_lowercase(), v.trim().to_string()));
    }
    let len: usize = headers.iter().find(|(k, _)| k == "content-length").map_or(0, |(_, v)| v.parse().unwrap());
    let mut body = vec![0; len];
    r.read_exact(&mut body).unwrap();
    (status.trim_end().to_string(), headers, String::from_utf8(body).unwrap())
}

#[test]
fn an_app_serves_over_http_until_it_stops() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("haneul_serve");
    std::fs::create_dir_all(dir.join("static")).unwrap();
    std::fs::write(dir.join("static").join("a.css"), "p { color: red; }").unwrap();
    let program = format!(
        "[하늘]에서 <앱>, <응답>을 가져오자\n\
         '앱'을 <앱>()으로 정하자\n\
         <인사>를 만들자 ('요청'):\n    틀\"안녕, {{'요청'의 <쿼리>(\"이름\", \"손님\")}}\"을 돌려주자\n\
         <폼>을 만들자 ('요청'):\n    틀\"{{'요청'의 <폼>(\"a\")}}+{{'요청'의 <폼>(\"b\")}}\"을 돌려주자\n\
         <멈춤>을 만들자 ():\n    '앱'의 <멈추기>()를 실행하자\n    <응답>(\"잘 가\", 202)를 돌려주자\n\
         '앱'의 <GET>(\"/\", <인사>)를 실행하자\n\
         '앱'의 <POST>(\"/폼\", <폼>)을 실행하자\n\
         '앱'의 <POST>(\"/멈춤\", <멈춤>)을 실행하자\n\
         \"시작\"을 출력하자\n\
         '앱'의 <실행>({PORT})을 실행하자\n\
         \"끝\"을 출력하자\n"
    );
    std::fs::write(dir.join("app.hr"), program).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_haru"))
        .args(["run", "app.hr"])
        .current_dir(&dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    // Two requests on one connection (keep-alive), then more.
    let s = connect();
    let mut w = s.try_clone().unwrap();
    let mut r = BufReader::new(s);
    w.write_all("GET /?%EC%9D%B4%EB%A6%84=%ED%95%98%EB%A3%A8 HTTP/1.1\r\nHost: x\r\n\r\n".as_bytes()).unwrap();
    let (status, headers, body) = read_answer(&mut r);
    assert_eq!((status.as_str(), body.as_str()), ("HTTP/1.1 200 OK", "안녕, 하루"));
    assert!(headers.contains(&("content-type".into(), "text/html; charset=utf-8".into())), "{headers:?}");
    w.write_all(b"POST /%ED%8F%BC HTTP/1.1\r\nHost: x\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 7\r\n\r\na=1&b=2").unwrap();
    let (status, _, body) = read_answer(&mut r);
    assert_eq!((status.as_str(), body.as_str()), ("HTTP/1.1 200 OK", "1+2"));
    w.write_all(b"GET /static/a.css HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    let (status, headers, body) = read_answer(&mut r);
    assert_eq!((status.as_str(), body.as_str()), ("HTTP/1.1 200 OK", "p { color: red; }"));
    assert!(headers.contains(&("content-type".into(), "text/css; charset=utf-8".into())), "{headers:?}");
    w.write_all(b"GET /nowhere HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    let (status, _, _) = read_answer(&mut r);
    assert_eq!(status, "HTTP/1.1 404 Not Found");
    w.write_all(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").unwrap();
    let (status, headers, _) = read_answer(&mut r);
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(headers.contains(&("connection".into(), "close".into())), "{headers:?}");

    let s = connect();
    let mut w = s.try_clone().unwrap();
    let mut r = BufReader::new(s);
    w.write_all("POST /멈춤 HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n".as_bytes()).unwrap();
    let (status, _, body) = read_answer(&mut r);
    assert_eq!((status.as_str(), body.as_str()), ("HTTP/1.1 202 Accepted", "잘 가"));

    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"), "시작\n끝\n");
    let log = String::from_utf8_lossy(&out.stderr);
    assert!(log.contains(&format!("http://127.0.0.1:{PORT}")), "{log}");
    assert!(log.contains("\"GET /static/a.css HTTP/1.1\" 200"), "{log}");
}
