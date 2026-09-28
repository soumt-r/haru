//! The network side, on `haru-http`'s plumbing (connections on other
//! threads, plain data only): what 하늘 does its own way is how it reads
//! (limits, refusals) and writes (`Server: haneul`, RFC 9110's wording).

use std::io::{self, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};

pub use haru_http::{accept_loop, wake, Job, Request as Incoming, Response as Answer};
use haru_http::Rules;

use crate::util::status_text;

/// How 하늘 reads and writes.
pub static RULES: Rules = Rules {
    max_line: Some(16 << 10),
    max_headers: Some(200),
    max_body: 16 << 20,
    truncate_body: false,
    strict_length: true,
    read_timeout: Some(std::time::Duration::from_secs(30)),
    target_ok: |t| t.starts_with('/'),
    refuse,
    write: write_answer,
};

pub fn bind(host: &str, port: u16) -> io::Result<TcpListener> {
    let addr = (host.trim_start_matches('[').trim_end_matches(']'), port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such host"))?;
    TcpListener::bind(addr)
}

/// A request it will not read (400, 413, 431): said in plain text.
fn refuse(w: &mut TcpStream, status: u16) -> io::Result<()> {
    let a = Answer {
        status,
        headers: vec![("Content-Type".into(), "text/plain; charset=utf-8".into())],
        body: format!("{status} {}\n", status_text(status)).into_bytes(),
    };
    write_answer(w, &a, false, true)
}

/// Writes an answer (without its body for `HEAD`, 1xx, 204 and 304).
fn write_answer(w: &mut TcpStream, a: &Answer, head: bool, close: bool) -> io::Result<()> {
    let bodiless = haru_http::bodiless(a.status);
    let mut out = format!("HTTP/1.1 {} {}\r\n", a.status, status_text(a.status)).into_bytes();
    let has = |name: &str| a.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name));
    for (k, v) in &a.headers {
        if !haru_http::header_name_ok(k) {
            continue;
        }
        let v = v.replace(['\r', '\n'], " ");
        out.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
    }
    if !bodiless && !has("Content-Length") {
        out.extend_from_slice(format!("Content-Length: {}\r\n", a.body.len()).as_bytes());
    }
    if !has("Date") {
        out.extend_from_slice(format!("Date: {}\r\n", haru_http::http_date(haru_http::now_secs())).as_bytes());
    }
    if !has("Server") {
        out.extend_from_slice(b"Server: haneul\r\n");
    }
    if close {
        out.extend_from_slice(b"Connection: close\r\n");
    }
    out.extend_from_slice(b"\r\n");
    if !head && !bodiless {
        out.extend_from_slice(&a.body);
    }
    w.write_all(&out)?;
    w.flush()
}
