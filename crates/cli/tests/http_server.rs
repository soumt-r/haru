//! The bundled http_server package: `haru run` serves, a client here asks,
//! and the program ends after `<서버닫기>`. Answers are Hana's (compared by
//! hand with Hana's Go server).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const PORT: u16 = 47291;

/// One request on a fresh connection; the raw answer.
fn ask(request: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut s = loop {
        match TcpStream::connect(("127.0.0.1", PORT)) {
            Ok(s) => break s,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => panic!("no server: {e}"),
        }
    };
    s.write_all(request.as_bytes()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out
}

fn status_and_body(answer: &str) -> (String, String) {
    let (head, body) = answer.split_once("\r\n\r\n").unwrap();
    (head.lines().next().unwrap().to_string(), body.to_string())
}

#[test]
fn a_program_serves_until_it_closes_the_server() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("http_server");
    std::fs::create_dir_all(&dir).unwrap();
    let program = format!(
        "[http_server]에서 전부 가져오자\n\
         <인사>를 만들자 ('요청'):\n    (\"안녕, \" + (('요청'의 \"query\")의 \"이름\"))을 돌려주자\n\
         <목록>을 만들자 ('요청'):\n    [1, 2]를 돌려주자\n\
         <멈춤>을 만들자 ('요청'):\n    <서버닫기>()를 실행하자\n    <응답>(202, \"잘 가\")를 돌려주자\n\
         <GET>(\"/hi\", <인사>)를 실행하자\n\
         <GET>(\"/list\", <목록>)을 실행하자\n\
         <POST>(\"/stop\", <멈춤>)을 실행하자\n\
         <서버열기>({PORT})를 실행하자\n\
         \"끝\"을 출력하자\n"
    );
    std::fs::write(dir.join("server.hr"), program).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_haru"))
        .args(["run", "server.hr"])
        .current_dir(&dir)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();

    let close = "Connection: close\r\n\r\n";
    let (status, body) = status_and_body(&ask(&format!("GET /hi?%EC%9D%B4%EB%A6%84=%ED%95%98%EB%A3%A8 HTTP/1.1\r\nHost: x\r\n{close}")));
    assert_eq!((status.as_str(), body.as_str()), ("HTTP/1.1 200 OK", "안녕, 하루"));
    let (status, body) = status_and_body(&ask(&format!("GET /list HTTP/1.1\r\nHost: x\r\n{close}")));
    assert_eq!((status.as_str(), body.as_str()), ("HTTP/1.1 500 Internal Server Error", "the handler must return a string or a response\n"));
    let (status, body) = status_and_body(&ask(&format!("GET /nowhere HTTP/1.1\r\nHost: x\r\n{close}")));
    assert_eq!((status.as_str(), body.as_str()), ("HTTP/1.1 404 Not Found", "404 page not found\n"));
    let (status, body) = status_and_body(&ask(&format!("POST /stop HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n{close}")));
    assert_eq!((status.as_str(), body.as_str()), ("HTTP/1.1 202 Accepted", "잘 가"));

    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"), "끝\n");
}
