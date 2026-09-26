//! The native half of the http_server package, answering as Hana's Go
//! library (net/http behind JSON) does.
//!
//! Hana runs handlers from the server's goroutines under its execution lock.
//! Here the program's values never leave its thread: `<네이티브_Listen>` opens
//! the port, other threads read requests and hand them over as plain data,
//! and the waiting Listen runs each handler on the program's thread, then
//! hands the answer back to be written. Hana passes everything through JSON,
//! so the same conversions happen here (see `J`).

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::Duration;

use haru_sdk::abi::tag;
use haru_sdk::prelude::*;

fn build(m: &mut Module) {
    m.raw("Route", route);
    m.raw("Listen", listen);
    m.raw("Shutdown", shutdown);
}

haru_sdk::export!("http_server", build);

// ---------------------------------------------------------------------------
// JSON, as the values pass through it between Hana and Go

/// A value as JSON holds it (object keys sorted, as Hana writes them).
#[derive(Clone, Debug)]
enum J {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    List(Vec<J>),
    Obj(Vec<(String, J)>),
}

fn unsupported() -> Error {
    Error::new("ValueError.JSONUnsupported")
}

/// Hana's `ToJSON` (with `wrap`: function values travel as `{"$fn": id}`).
fn to_json(v: &Value, wrap: bool, depth: usize) -> Result<J> {
    Ok(match v.tag() {
        tag::NULL => J::Null,
        tag::BOOL => J::Bool(v.as_bool().unwrap()),
        tag::NUM => {
            let n = v.as_num().unwrap();
            if !n.is_finite() {
                return Err(unsupported());
            }
            // JSON has no -0 (Hana writes "0").
            J::Num(if n == 0.0 { 0.0 } else { n })
        }
        tag::STR => J::Str(v.as_str().unwrap().to_string()),
        tag::FUNC if wrap => J::Obj(vec![("$fn".into(), J::Str("cb".into()))]),
        tag::LIST if depth <= 1000 => {
            J::List(v.as_list().unwrap().iter().map(|x| to_json(&x, wrap, depth + 1)).collect::<Result<_>>()?)
        }
        tag::DICT => {
            let d = v.as_dict().unwrap();
            let mut entries = Vec::new();
            for k in d.keys().iter() {
                let name = k.as_str().ok_or_else(unsupported)?.to_string();
                entries.push((name, to_json(&d.get(&k).unwrap_or(Value::NULL), wrap, depth + 1)?));
            }
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            J::Obj(entries)
        }
        _ => return Err(unsupported()),
    })
}

/// Go's `fmt.Sprint` of what `encoding/json` decodes into `interface{}`.
fn go_sprint(j: &J) -> String {
    match j {
        J::Null => "<nil>".into(),
        J::Bool(b) => b.to_string(),
        J::Num(n) => go_float(*n),
        J::Str(s) => s.clone(),
        J::List(items) => format!("[{}]", items.iter().map(go_sprint).collect::<Vec<_>>().join(" ")),
        J::Obj(entries) => format!("map[{}]", entries.iter().map(|(k, v)| format!("{k}:{}", go_sprint(v))).collect::<Vec<_>>().join(" ")),
    }
}

/// Go's `%v` of a float64 (strconv's shortest 'g').
fn go_float(f: f64) -> String {
    if f == 0.0 {
        return if f.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let e = format!("{f:e}");
    let (mantissa, exp) = e.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    if !(-4..6).contains(&exp) {
        let sign = if exp < 0 { '-' } else { '+' };
        return format!("{mantissa}e{sign}{:02}", exp.abs());
    }
    format!("{f}")
}

/// Go's `int(f)` on amd64: out of range is the smallest int.
fn go_int(f: f64) -> i64 {
    if f.is_nan() || f >= 9.223372036854775807e18 || f < -9.223372036854775808e18 {
        i64::MIN
    } else {
        f as i64
    }
}

/// A Go string made from bytes as `encoding/json` writes it: each byte that
/// is not valid UTF-8 becomes U+FFFD.
fn json_string(mut b: &[u8]) -> String {
    let mut out = String::with_capacity(b.len());
    loop {
        match std::str::from_utf8(b) {
            Ok(s) => {
                out.push_str(s);
                return out;
            }
            Err(e) => {
                let (good, rest) = b.split_at(e.valid_up_to());
                out.push_str(std::str::from_utf8(good).unwrap());
                let bad = e.error_len().unwrap_or(rest.len());
                for _ in 0..bad {
                    out.push('\u{fffd}');
                }
                b = &rest[bad..];
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The program's side: routes and the waiting Listen

thread_local! {
    /// "METHOD path" -> handler.
    static ROUTES: RefCell<BTreeMap<String, Value>> = const { RefCell::new(BTreeMap::new()) };
    /// The running server's stop switch (set by Shutdown).
    static RUNNING: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
    static SERVING: Cell<bool> = const { Cell::new(false) };
}

fn fail(name: &str, message: impl Into<String>) -> Error {
    Error::new("ImportError.NativeCallFailed").arg(name).arg(message.into())
}

fn json_args(args: &[Value]) -> Result<Vec<J>> {
    args.iter().map(|a| to_json(a, true, 0)).collect()
}

/// A JSON value read as a Go string (null is "").
fn as_go_string(j: &J) -> Option<String> {
    match j {
        J::Null => Some(String::new()),
        J::Str(s) => Some(s.clone()),
        _ => None,
    }
}

/// Go's `strings.ToUpper`, one character for one.
fn go_upper(s: &str) -> String {
    s.chars()
        .map(|c| {
            let mut up = c.to_uppercase();
            match (up.next(), up.next()) {
                (Some(u), None) => u,
                _ => c,
            }
        })
        .collect()
}

/// `<네이티브_Route>(method, path, handler)`.
fn route(args: &[Value]) -> Result<Value> {
    let j = json_args(args)?;
    if j.len() != 3 {
        return Err(fail("Route", "Route needs (method, path, handler)"));
    }
    let bad = || fail("Route", "Route needs a method, a path and a function");
    let method = as_go_string(&j[0]).ok_or_else(bad)?;
    let path = as_go_string(&j[1]).ok_or_else(bad)?;
    if args[2].tag() != tag::FUNC {
        return Err(bad());
    }
    ROUTES.with(|r| r.borrow_mut().insert(format!("{} {path}", go_upper(&method)), args[2].clone()));
    Ok(Value::NULL)
}

/// `<네이티브_Shutdown>()`: the server stops; Listen then returns.
fn shutdown(_: &[Value]) -> Result<Value> {
    RUNNING.with(|r| {
        if let Some(stop) = r.borrow().as_ref() {
            stop.store(true, Ordering::SeqCst);
        }
    });
    Ok(Value::NULL)
}

/// A request as the reading thread sends it over.
struct Request {
    method: String,
    path: String,
    query: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    remote: String,
}

/// What the program's thread sends back to be written.
struct Answer {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    /// Content-Length is left out (1xx, 204, 304).
    bodiless: bool,
}

struct Job {
    request: Request,
    reply: Sender<Answer>,
}

/// `<네이티브_Listen>(port)`: serves until Shutdown.
fn listen(args: &[Value]) -> Result<Value> {
    let j = json_args(args)?;
    if j.len() != 1 {
        return Err(fail("Listen", "Listen needs (port)"));
    }
    let addr = match &j[0] {
        J::Null => ":0".to_string(),
        J::Num(n) => format!(":{}", go_int(*n)),
        J::Str(s) if s.contains(':') => s.clone(),
        J::Str(s) => format!(":{s}"),
        _ => return Err(fail("Listen", "the port must be a number or an address")),
    };
    if SERVING.with(Cell::get) {
        return Err(fail("Listen", "the server is already running"));
    }
    let listener = bind(&addr).map_err(|m| fail("Listen", m))?;
    let stop = Arc::new(AtomicBool::new(false));
    let (jobs, incoming) = channel::<Job>();
    let local = listener.local_addr().ok();
    {
        let stop = stop.clone();
        std::thread::spawn(move || accept_loop(listener, jobs, stop));
    }
    RUNNING.with(|r| *r.borrow_mut() = Some(stop.clone()));
    SERVING.with(|s| s.set(true));
    haru_sdk::flush_output();
    serve(&incoming, &stop);
    SERVING.with(|s| s.set(false));
    RUNNING.with(|r| *r.borrow_mut() = None);
    // Wake the accepting thread so it sees the switch and ends.
    if let Some(a) = local {
        let wake = if a.ip().is_unspecified() { SocketAddr::new(std::net::Ipv4Addr::LOCALHOST.into(), a.port()) } else { a };
        let _ = TcpStream::connect_timeout(&wake, Duration::from_millis(200));
    }
    Ok(Value::NULL)
}

/// Runs handlers on the program's thread until the server is stopped.
fn serve(incoming: &Receiver<Job>, stop: &AtomicBool) {
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        match incoming.recv_timeout(Duration::from_millis(50)) {
            Ok(job) => {
                let answer = handle(job.request);
                let _ = job.reply.send(answer);
                // What the handler printed shows now, not at the next print.
                haru_sdk::flush_output();
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Go's http.Error: the message on its own line, plain text, not sniffed.
fn error_answer(message: &str, status: u16) -> Answer {
    Answer {
        status,
        headers: vec![
            ("Content-Type".into(), "text/plain; charset=utf-8".into()),
            ("X-Content-Type-Options".into(), "nosniff".into()),
        ],
        body: format!("{message}\n").into_bytes(),
        bodiless: false,
    }
}

fn handle(req: Request) -> Answer {
    let key = format!("{} {}", req.method, req.path);
    let Some(handler) = ROUTES.with(|r| r.borrow().get(&key).cloned()) else {
        return error_answer("404 page not found", 404);
    };
    let request = match request_value(&req) {
        Ok(v) => v,
        Err(_) => return error_answer("bad request", 500),
    };
    // The handler runs here; what it throws is its message in English, as
    // Hana's host callback reports it.
    let result = match handler.call_catching(&[request], 0) {
        Ok(v) => v,
        Err(message) => return error_answer(&message, 500),
    };
    let json = match to_json(&result, false, 0) {
        Ok(j) => j,
        Err(_) => return error_answer("The value contains something JSON cannot represent.", 500),
    };
    response_of(&json)
}

/// The request as the handler gets it (every text as JSON carries it).
fn request_value(r: &Request) -> Result<Value> {
    let query = Dict::new();
    for (k, v) in &r.query {
        query.set(k.as_str(), v.as_str())?;
    }
    let headers = Dict::new();
    for (k, v) in &r.headers {
        headers.set(k.as_str(), v.as_str())?;
    }
    let d = Dict::new();
    d.set("method", r.method.as_str())?;
    d.set("path", r.path.as_str())?;
    d.set("query", query)?;
    d.set("headers", headers)?;
    d.set("body", json_string(&r.body))?;
    d.set("remote", r.remote.as_str())?;
    haru_sdk::IntoRet::into_ret(d)
}

/// Go's writeResponse: null (an empty 200), a text (text/plain), or a
/// response {"status", "body", "type", "headers"} decoded as Go decodes it.
fn response_of(j: &J) -> Answer {
    let plain = |body: String| Answer {
        status: 200,
        headers: vec![("Content-Type".into(), "text/plain; charset=utf-8".into())],
        body: body.into_bytes(),
        bodiless: false,
    };
    let shape_error = || error_answer("the handler must return a string or a response", 500);
    let entries = match j {
        J::Null => return Answer { status: 200, headers: Vec::new(), body: Vec::new(), bodiless: false },
        J::Str(s) => return plain(s.clone()),
        J::Obj(e) => e,
        _ => return shape_error(),
    };
    // encoding/json matches fields ignoring case; a later key wins.
    let (mut status, mut body, mut ctype, mut headers) = (None::<f64>, String::new(), String::new(), None::<Vec<(String, J)>>);
    let mut bad = false;
    for (k, v) in entries {
        match k.to_lowercase().as_str() {
            "status" => match v {
                J::Num(n) => status = Some(*n),
                J::Null => {}
                _ => bad = true,
            },
            "body" => match as_go_string(v) {
                Some(s) if !matches!(v, J::Null) => body = s,
                Some(_) => {}
                None => bad = true,
            },
            "type" => match as_go_string(v) {
                Some(s) if !matches!(v, J::Null) => ctype = s,
                Some(_) => {}
                None => bad = true,
            },
            "headers" => match v {
                J::Obj(h) => headers = Some(h.clone()),
                J::Null => {}
                _ => bad = true,
            },
            _ => {}
        }
    }
    if bad {
        return shape_error();
    }
    let mut out: Vec<(String, String)> = Vec::new();
    fn set(out: &mut Vec<(String, String)>, k: &str, v: String) {
        let key = canonical(k);
        out.retain(|(x, _)| *x != key);
        out.push((key, v));
    }
    for (k, v) in headers.unwrap_or_default() {
        set(&mut out, &k, go_sprint(&v));
    }
    if !ctype.is_empty() {
        set(&mut out, "Content-Type", ctype);
    } else if !out.iter().any(|(k, _)| k == "Content-Type") {
        set(&mut out, "Content-Type", "text/plain; charset=utf-8".into());
    }
    let code = status.map_or(200, go_int);
    if !(100..=999).contains(&code) {
        // Go's http.Error keeps the headers already set.
        let mut a = error_answer(&format!("{code} is not an HTTP status"), 500);
        for (k, v) in out {
            if !a.headers.iter().any(|(x, _)| *x == k) {
                a.headers.push((k, v));
            }
        }
        return a;
    }
    let bodiless = (100..200).contains(&code) || code == 204 || code == 304;
    Answer { status: code as u16, headers: out, body: if bodiless { Vec::new() } else { body.into_bytes() }, bodiless }
}

/// Go's `CanonicalMIMEHeaderKey` (a key with other characters is kept as it is).
fn canonical(k: &str) -> String {
    let valid = |c: u8| c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c);
    if !k.bytes().all(valid) {
        return k.to_string();
    }
    let mut upper = true;
    k.bytes()
        .map(|c| {
            let c = if upper { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() };
            upper = c == b'-';
            c as char
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The network side (other threads; plain data only)

/// Go's English wording of the operating system's refusal.
fn os_message(e: &io::Error) -> String {
    match e.raw_os_error() {
        Some(10048) => "Only one usage of each socket address (protocol/network address/port) is normally permitted.".into(),
        Some(10013) => "An attempt was made to access a socket in a way forbidden by its access permissions.".into(),
        Some(10049) => "The requested address is not valid in its context.".into(),
        Some(98) => "address already in use".into(),
        Some(13) => "permission denied".into(),
        Some(99) => "cannot assign requested address".into(),
        _ => e.to_string(),
    }
}

/// Go's `net.Listen("tcp", addr)`: an empty host is every interface.
fn bind(addr: &str) -> std::result::Result<TcpListener, String> {
    let (host, port) = addr.rsplit_once(':').ok_or_else(|| format!("listen tcp {addr}: missing port in address"))?;
    let port: u16 = port.parse().map_err(|_| format!("listen tcp: address {port}: invalid port"))?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() {
        // IPv6 and IPv4 at once, as Go listens on ":port".
        let dual = (|| -> io::Result<TcpListener> {
            let s = socket2::Socket::new(socket2::Domain::IPV6, socket2::Type::STREAM, None)?;
            s.set_only_v6(false)?;
            s.set_reuse_address(false)?;
            s.bind(&SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, port)).into())?;
            s.listen(128)?;
            Ok(s.into())
        })();
        return match dual {
            Ok(l) => Ok(l),
            Err(e) if e.kind() == io::ErrorKind::AddrInUse || e.raw_os_error() == Some(10013) => {
                Err(format!("listen tcp {addr}: bind: {}", os_message(&e)))
            }
            Err(_) => TcpListener::bind(("0.0.0.0", port)).map_err(|e| format!("listen tcp {addr}: bind: {}", os_message(&e))),
        };
    }
    let target = (host, port).to_socket_addrs().map_err(|e| format!("listen tcp {addr}: {}", os_message(&e)))?.next();
    let target = target.ok_or_else(|| format!("listen tcp {addr}: no such host"))?;
    TcpListener::bind(target).map_err(|e| format!("listen tcp {addr}: bind: {}", os_message(&e)))
}

fn accept_loop(listener: TcpListener, jobs: Sender<Job>, stop: Arc<AtomicBool>) {
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

const MAX_BODY: usize = 10 << 20;

/// One connection: requests in, answers out, until either side stops.
fn connection(stream: TcpStream, jobs: Sender<Job>, stop: Arc<AtomicBool>) -> io::Result<()> {
    let remote = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split(' ').collect();
        if parts.len() != 3 || !parts[2].starts_with("HTTP/1.") {
            writer.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain; charset=utf-8\r\nConnection: close\r\n\r\n400 Bad Request")?;
            return Ok(());
        }
        let (method, target, version) = (parts[0].to_string(), parts[1], parts[2]);
        let mut headers: Vec<(String, String)> = Vec::new();
        loop {
            let mut h = String::new();
            if reader.read_line(&mut h)? == 0 {
                return Ok(());
            }
            let h = h.trim_end_matches(['\r', '\n']);
            if h.is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                headers.push((k.trim().to_string(), v.trim().to_string()));
            }
        }
        let header = |name: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone());
        let Some((path, query)) = split_target(target) else {
            writer.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain; charset=utf-8\r\nConnection: close\r\n\r\n400 Bad Request")?;
            return Ok(());
        };
        if header("Expect").is_some_and(|e| e.eq_ignore_ascii_case("100-continue")) {
            writer.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
        }
        let chunked = header("Transfer-Encoding").is_some_and(|t| t.to_ascii_lowercase().contains("chunked"));
        let mut body = if chunked {
            read_chunked(&mut reader)?
        } else {
            let n: usize = header("Content-Length").and_then(|l| l.parse().ok()).unwrap_or(0);
            let mut b = vec![0; n];
            reader.read_exact(&mut b)?;
            b
        };
        body.truncate(MAX_BODY);
        let close = header("Connection").is_some_and(|c| c.eq_ignore_ascii_case("close"))
            || (version == "HTTP/1.0" && !header("Connection").is_some_and(|c| c.eq_ignore_ascii_case("keep-alive")));

        // Go moves Host and Transfer-Encoding out of the header map; names
        // are lower-case and the first value of each counts.
        let mut seen: Vec<(String, String)> = Vec::new();
        for (k, v) in &headers {
            let key = k.to_ascii_lowercase();
            if key == "host" || key == "transfer-encoding" || seen.iter().any(|(s, _)| *s == key) {
                continue;
            }
            seen.push((key, json_string(v.as_bytes())));
        }
        let head = method == "HEAD";
        let request = Request { method, path, query, headers: seen, body, remote: remote.clone() };
        let (reply, answer) = channel();
        if stop.load(Ordering::SeqCst) || jobs.send(Job { request, reply }).is_err() {
            return Ok(());
        }
        let Ok(a) = answer.recv() else { return Ok(()) };
        write_answer(&mut writer, &a, head, close)?;
        if close {
            return Ok(());
        }
    }
}

/// The decoded path and the query's first values (Go's URL rules).
fn split_target(target: &str) -> Option<(String, Vec<(String, String)>)> {
    let (raw_path, raw_query) = target.split_once('?').unwrap_or((target, ""));
    let raw_path = match raw_path.find("://") {
        // An absolute URL: its path.
        Some(i) => raw_path[i + 3..].find('/').map_or("/", |j| &raw_path[i + 3 + j..]),
        None => raw_path,
    };
    let path = unescape(raw_path, false)?;
    let mut query: Vec<(String, String)> = Vec::new();
    for pair in raw_query.split('&') {
        if pair.is_empty() || pair.contains(';') {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let (Some(k), Some(v)) = (unescape(k, true), unescape(v, true)) else { continue };
        if !query.iter().any(|(x, _)| *x == k) {
            query.push((k, v));
        }
    }
    Some((path, query))
}

/// %XX decoding (`plus`: '+' is a space, as in a query).
fn unescape(s: &str, plus: bool) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let hex = |c: u8| (c as char).to_digit(16);
                let (h, l) = (hex(*b.get(i + 1)?)?, hex(*b.get(i + 2)?)?);
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            b'+' if plus => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    Some(json_string(&out))
}

fn read_chunked(r: &mut BufReader<TcpStream>) -> io::Result<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let mut line = String::new();
        r.read_line(&mut line)?;
        let size_text = line.trim().split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
        if size == 0 {
            // Trailers, up to the blank line.
            loop {
                let mut t = String::new();
                if r.read_line(&mut t)? == 0 || t.trim().is_empty() {
                    return Ok(body);
                }
            }
        }
        let mut chunk = vec![0; size];
        r.read_exact(&mut chunk)?;
        if body.len() < MAX_BODY {
            body.extend_from_slice(&chunk);
        }
        let mut crlf = String::new();
        r.read_line(&mut crlf)?;
    }
}

/// Go's `http.StatusText`.
fn status_text(code: u16) -> Option<&'static str> {
    Some(match code {
        100 => "Continue",
        101 => "Switching Protocols",
        102 => "Processing",
        103 => "Early Hints",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        203 => "Non-Authoritative Information",
        204 => "No Content",
        205 => "Reset Content",
        206 => "Partial Content",
        207 => "Multi-Status",
        208 => "Already Reported",
        226 => "IM Used",
        300 => "Multiple Choices",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        305 => "Use Proxy",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        402 => "Payment Required",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        407 => "Proxy Authentication Required",
        408 => "Request Timeout",
        409 => "Conflict",
        410 => "Gone",
        411 => "Length Required",
        412 => "Precondition Failed",
        413 => "Request Entity Too Large",
        414 => "Request URI Too Long",
        415 => "Unsupported Media Type",
        416 => "Requested Range Not Satisfiable",
        417 => "Expectation Failed",
        418 => "I'm a teapot",
        421 => "Misdirected Request",
        422 => "Unprocessable Entity",
        423 => "Locked",
        424 => "Failed Dependency",
        425 => "Too Early",
        426 => "Upgrade Required",
        428 => "Precondition Required",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        451 => "Unavailable For Legal Reasons",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        505 => "HTTP Version Not Supported",
        506 => "Variant Also Negotiates",
        507 => "Insufficient Storage",
        508 => "Loop Detected",
        510 => "Not Extended",
        511 => "Network Authentication Required",
        _ => return None,
    })
}

/// `http.TimeFormat` of now.
fn http_date() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    format!(
        "{}, {d:02} {} {y:04} {:02}:{:02}:{:02} GMT",
        DAYS[days.rem_euclid(7) as usize],
        MONTHS[(m - 1) as usize],
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

/// What Go writes back: small bodies with their length, larger ones in chunks.
fn write_answer(w: &mut TcpStream, a: &Answer, head: bool, close: bool) -> io::Result<()> {
    let text = status_text(a.status).map_or_else(|| format!("status code {}", a.status), str::to_string);
    let mut out = format!("HTTP/1.1 {:03} {text}\r\n", a.status).into_bytes();
    let mut fields: Vec<(String, String)> = a
        .headers
        .iter()
        .filter(|(k, _)| !k.is_empty() && k.bytes().all(|c| c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c)))
        .map(|(k, v)| (k.clone(), v.replace(['\r', '\n'], " ").trim_matches([' ', '\t']).to_string()))
        .collect();
    let chunked = !a.bodiless && a.body.len() > 2048;
    if !a.bodiless && !chunked {
        fields.push(("Content-Length".into(), a.body.len().to_string()));
    }
    if chunked {
        fields.push(("Transfer-Encoding".into(), "chunked".into()));
    }
    fields.push(("Date".into(), http_date()));
    if close {
        fields.push(("Connection".into(), "close".into()));
    }
    fields.sort_by(|x, y| x.0.cmp(&y.0));
    for (k, v) in &fields {
        out.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
    }
    out.extend_from_slice(b"\r\n");
    if !head && !a.bodiless {
        if chunked {
            out.extend_from_slice(format!("{:x}\r\n", a.body.len()).as_bytes());
            out.extend_from_slice(&a.body);
            out.extend_from_slice(b"\r\n0\r\n\r\n");
        } else {
            out.extend_from_slice(&a.body);
        }
    }
    w.write_all(&out)?;
    w.flush()
}
