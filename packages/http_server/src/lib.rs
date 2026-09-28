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
use std::io::{self, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::Duration;

use haru_http::{Job, Request, Response, Rules};
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
        std::thread::spawn(move || haru_http::accept_loop(listener, jobs, stop, &RULES));
    }
    RUNNING.with(|r| *r.borrow_mut() = Some(stop.clone()));
    SERVING.with(|s| s.set(true));
    haru_sdk::flush_output();
    serve(&incoming, &stop);
    SERVING.with(|s| s.set(false));
    RUNNING.with(|r| *r.borrow_mut() = None);
    // Wake the accepting thread so it sees the switch and ends.
    if let Some(a) = local {
        haru_http::wake(a);
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
fn error_answer(message: &str, status: u16) -> Response {
    Response {
        status,
        headers: vec![
            ("Content-Type".into(), "text/plain; charset=utf-8".into()),
            ("X-Content-Type-Options".into(), "nosniff".into()),
        ],
        body: format!("{message}\n").into_bytes(),
    }
}

fn handle(req: Request) -> Response {
    // `target_ok` let only targets `split_target` reads through.
    let (path, query) = split_target(&req.target).unwrap_or_else(|| ("/".into(), Vec::new()));
    let key = format!("{} {path}", req.method);
    let Some(handler) = ROUTES.with(|r| r.borrow().get(&key).cloned()) else {
        return error_answer("404 page not found", 404);
    };
    let request = match request_value(&req, &path, &query) {
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
fn request_value(r: &Request, path: &str, query: &[(String, String)]) -> Result<Value> {
    let q = Dict::new();
    for (k, v) in query {
        q.set(k.as_str(), v.as_str())?;
    }
    // Go moves Host and Transfer-Encoding out of the header map; names are
    // lower-case and the first value of each counts.
    let headers = Dict::new();
    let mut seen: Vec<String> = Vec::new();
    for (k, v) in &r.headers {
        let key = k.to_ascii_lowercase();
        if key == "host" || key == "transfer-encoding" || seen.contains(&key) {
            continue;
        }
        headers.set(key.as_str(), json_string(v.as_bytes()).as_str())?;
        seen.push(key);
    }
    let d = Dict::new();
    d.set("method", r.method.as_str())?;
    d.set("path", path)?;
    d.set("query", q)?;
    d.set("headers", headers)?;
    d.set("body", json_string(&r.body))?;
    d.set("remote", r.remote.as_str())?;
    haru_sdk::IntoRet::into_ret(d)
}

/// Go's writeResponse: null (an empty 200), a text (text/plain), or a
/// response {"status", "body", "type", "headers"} decoded as Go decodes it.
fn response_of(j: &J) -> Response {
    let plain = |body: String| Response {
        status: 200,
        headers: vec![("Content-Type".into(), "text/plain; charset=utf-8".into())],
        body: body.into_bytes(),
    };
    let shape_error = || error_answer("the handler must return a string or a response", 500);
    let entries = match j {
        J::Null => return Response { status: 200, headers: Vec::new(), body: Vec::new() },
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
    let code = code as u16;
    Response { status: code, headers: out, body: if haru_http::bodiless(code) { Vec::new() } else { body.into_bytes() } }
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

/// How Go's server reads and writes, as far as the programs can tell: no
/// limits but a body cut at 10 MB, and its answers.
static RULES: Rules = Rules {
    max_line: None,
    max_headers: None,
    max_body: 10 << 20,
    truncate_body: true,
    strict_length: false,
    read_timeout: None,
    target_ok: |t| split_target(t).is_some(),
    refuse: bad_request,
    write: write_answer,
};

/// What Go writes for a request it cannot read.
fn bad_request(w: &mut TcpStream, _status: u16) -> io::Result<()> {
    w.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain; charset=utf-8\r\nConnection: close\r\n\r\n400 Bad Request")?;
    w.flush()
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

/// What Go writes back: small bodies with their length, larger ones in chunks.
fn write_answer(w: &mut TcpStream, a: &Response, head: bool, close: bool) -> io::Result<()> {
    let bodiless = haru_http::bodiless(a.status);
    let text = status_text(a.status).map_or_else(|| format!("status code {}", a.status), str::to_string);
    let mut out = format!("HTTP/1.1 {:03} {text}\r\n", a.status).into_bytes();
    let mut fields: Vec<(String, String)> = a
        .headers
        .iter()
        .filter(|(k, _)| haru_http::header_name_ok(k))
        .map(|(k, v)| (k.clone(), v.replace(['\r', '\n'], " ").trim_matches([' ', '\t']).to_string()))
        .collect();
    let chunked = !bodiless && a.body.len() > 2048;
    if !bodiless && !chunked {
        fields.push(("Content-Length".into(), a.body.len().to_string()));
    }
    if chunked {
        fields.push(("Transfer-Encoding".into(), "chunked".into()));
    }
    fields.push(("Date".into(), haru_http::http_date(haru_http::now_secs())));
    if close {
        fields.push(("Connection".into(), "close".into()));
    }
    fields.sort_by(|x, y| x.0.cmp(&y.0));
    for (k, v) in &fields {
        out.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
    }
    out.extend_from_slice(b"\r\n");
    if !head && !bodiless {
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
