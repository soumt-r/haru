//! The network side: HTTP/1.1 on other threads, with plain data only. A
//! connection's thread reads a request, hands it to the program's thread as
//! a [`Job`] and writes back the [`Answer`] it gets.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::time::Duration;

use crate::util::{http_date, now_secs, status_text};

/// A request as it came.
pub struct Incoming {
    pub method: String,
    /// The path and query as the request line has them.
    pub target: String,
    pub version: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub remote: String,
}

/// What to write back.
pub struct Answer {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

pub struct Job {
    pub incoming: Incoming,
    pub reply: Sender<Answer>,
}

const MAX_BODY: usize = 16 << 20;
const MAX_LINE: u64 = 16 << 10;
const MAX_HEADERS: usize = 200;

pub fn bind(host: &str, port: u16) -> io::Result<TcpListener> {
    let addr = (host.trim_start_matches('[').trim_end_matches(']'), port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such host"))?;
    TcpListener::bind(addr)
}

/// Accepts connections until `stop` is set (the program's thread then
/// connects once to wake this up).
pub fn accept_loop(listener: TcpListener, jobs: Sender<Job>, stop: Arc<AtomicBool>) {
    for conn in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        if let Ok(c) = conn {
            let jobs = jobs.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                let _ = connection(c, jobs, stop);
            });
        }
    }
}

/// Connects to the listening address so a waiting `accept` returns.
pub fn wake(local: SocketAddr) {
    let addr = if local.ip().is_unspecified() {
        let ip: std::net::IpAddr = if local.is_ipv4() { std::net::Ipv4Addr::LOCALHOST.into() } else { std::net::Ipv6Addr::LOCALHOST.into() };
        SocketAddr::new(ip, local.port())
    } else {
        local
    };
    let _ = TcpStream::connect_timeout(&addr, Duration::from_millis(200));
}

fn read_line(r: &mut BufReader<TcpStream>) -> io::Result<Option<String>> {
    let mut line = Vec::new();
    let n = r.by_ref().take(MAX_LINE).read_until(b'\n', &mut line)?;
    if n == 0 {
        return Ok(None);
    }
    if !line.ends_with(b"\n") {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
    }
    while matches!(line.last(), Some(b'\n' | b'\r')) {
        line.pop();
    }
    Ok(Some(String::from_utf8_lossy(&line).into_owned()))
}

fn plain(status: u16, message: &str) -> Answer {
    Answer {
        status,
        headers: vec![("Content-Type".into(), "text/plain; charset=utf-8".into())],
        body: format!("{status} {message}\n").into_bytes(),
    }
}

fn connection(stream: TcpStream, jobs: Sender<Job>, stop: Arc<AtomicBool>) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let remote = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    loop {
        let Some(mut line) = read_line(&mut reader)? else { return Ok(()) };
        // Blank lines before a request are allowed.
        while line.is_empty() {
            match read_line(&mut reader)? {
                Some(l) => line = l,
                None => return Ok(()),
            }
        }
        let parts: Vec<&str> = line.split(' ').collect();
        if parts.len() != 3 || !parts[2].starts_with("HTTP/1.") || parts[0].is_empty() || !parts[1].starts_with('/') {
            write_answer(&mut writer, &plain(400, "Bad Request"), false, true)?;
            return Ok(());
        }
        let (method, target, version) = (parts[0].to_string(), parts[1].to_string(), parts[2].to_string());
        let mut headers: Vec<(String, String)> = Vec::new();
        loop {
            let Some(h) = read_line(&mut reader)? else { return Ok(()) };
            if h.is_empty() {
                break;
            }
            if headers.len() >= MAX_HEADERS {
                write_answer(&mut writer, &plain(431, "Request Header Fields Too Large"), false, true)?;
                return Ok(());
            }
            if let Some((k, v)) = h.split_once(':') {
                headers.push((k.trim().to_string(), v.trim().to_string()));
            }
        }
        let header = |name: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str());
        let close = header("Connection").is_some_and(|c| c.eq_ignore_ascii_case("close"))
            || (version == "HTTP/1.0" && !header("Connection").is_some_and(|c| c.eq_ignore_ascii_case("keep-alive")));
        if header("Expect").is_some_and(|e| e.eq_ignore_ascii_case("100-continue")) {
            writer.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
        }
        let chunked = header("Transfer-Encoding").is_some_and(|t| t.to_ascii_lowercase().contains("chunked"));
        let body = if chunked {
            match read_chunked(&mut reader)? {
                Some(b) => b,
                None => {
                    write_answer(&mut writer, &plain(413, "Content Too Large"), false, true)?;
                    return Ok(());
                }
            }
        } else {
            let n: usize = match header("Content-Length").map(|l| l.parse()) {
                None => 0,
                Some(Ok(n)) => n,
                Some(Err(_)) => {
                    write_answer(&mut writer, &plain(400, "Bad Request"), false, true)?;
                    return Ok(());
                }
            };
            if n > MAX_BODY {
                write_answer(&mut writer, &plain(413, "Content Too Large"), false, true)?;
                return Ok(());
            }
            let mut b = vec![0; n];
            reader.read_exact(&mut b)?;
            b
        };
        let head = method == "HEAD";
        let incoming = Incoming { method, target, version, headers, body, remote: remote.clone() };
        let (reply, answer) = channel();
        if stop.load(Ordering::SeqCst) || jobs.send(Job { incoming, reply }).is_err() {
            return Ok(());
        }
        let Ok(a) = answer.recv() else { return Ok(()) };
        write_answer(&mut writer, &a, head, close)?;
        if close {
            return Ok(());
        }
    }
}

/// A chunked body (`None` when it is too large).
fn read_chunked(r: &mut BufReader<TcpStream>) -> io::Result<Option<Vec<u8>>> {
    let mut body = Vec::new();
    loop {
        let line = read_line(r)?.ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        let size_text = line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
        if size == 0 {
            // Trailers, up to the blank line.
            while let Some(t) = read_line(r)? {
                if t.is_empty() {
                    break;
                }
            }
            return Ok(Some(body));
        }
        if body.len() + size > MAX_BODY {
            return Ok(None);
        }
        let start = body.len();
        body.resize(start + size, 0);
        r.read_exact(&mut body[start..])?;
        read_line(r)?;
    }
}

/// Writes an answer (without its body for `HEAD`, 1xx, 204 and 304).
pub fn write_answer(w: &mut TcpStream, a: &Answer, head: bool, close: bool) -> io::Result<()> {
    let bodiless = (100..200).contains(&a.status) || a.status == 204 || a.status == 304;
    let mut out = format!("HTTP/1.1 {} {}\r\n", a.status, status_text(a.status)).into_bytes();
    let has = |name: &str| a.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name));
    for (k, v) in &a.headers {
        // A header can never break the answer's lines.
        if k.is_empty() || !k.bytes().all(|c| c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c)) {
            continue;
        }
        let v = v.replace(['\r', '\n'], " ");
        out.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
    }
    if !bodiless && !has("Content-Length") {
        out.extend_from_slice(format!("Content-Length: {}\r\n", a.body.len()).as_bytes());
    }
    if !has("Date") {
        out.extend_from_slice(format!("Date: {}\r\n", http_date(now_secs())).as_bytes());
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
