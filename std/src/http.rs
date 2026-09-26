//! [HTTP] / 【HTTP】, as Hana implements it (`std/stdimpl/http.go`): HTTP/1.1
//! spoken directly over a connection (TLS for https), one request and one
//! answer per connection, redirects followed.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use haru_sdk::prelude::*;
use haru_sdk::IntoRet;
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, StreamOwned};

use crate::gourl::Url;
use crate::hana::{between, string};
use crate::net::{access, dial, join_host_port, Conn, Timed, DEFAULT_TIMEOUT};

haru_sdk::entry!(pub(crate) fn entry = "http", build);

fn build(m: &mut Module) {
    crate::describe(m, "http", &[("http.get", get), ("http.post", post), ("http.request", request)]);
}

const MAX_BODY: usize = 32 << 20;
const MAX_REDIRECTS: usize = 10;
const METHODS: [&str; 6] = ["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD"];

/// The optional dictionary of request headers (text to text), in the
/// dictionary's order.
fn headers(args: &[Value], i: usize) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    if i >= args.len() {
        return Ok(out);
    }
    let dict = args[i].as_dict().ok_or_else(|| Error::new("TypeError.NativeArgDict").arg((i + 1) as f64))?;
    for k in dict.keys().iter() {
        let v = dict.get(&k).unwrap_or(Value::NULL);
        match (k.as_str(), v.as_str()) {
            (Some(k), Some(v)) => out.push((k.to_string(), v.to_string())),
            _ => return Err(Error::new("TypeError.NativeDictStrings").arg((i + 1) as f64)),
        }
    }
    Ok(out)
}

fn get(args: &[Value]) -> Result<Value> {
    between(args, 1, 2)?;
    let target = string(args, 0)?;
    let h = headers(args, 1)?;
    send("GET".into(), &target, String::new(), &h)
}

fn post(args: &[Value]) -> Result<Value> {
    between(args, 2, 3)?;
    let target = string(args, 0)?;
    let body = string(args, 1)?.to_string();
    let h = headers(args, 2)?;
    send("POST".into(), &target, body, &h)
}

fn request(args: &[Value]) -> Result<Value> {
    between(args, 2, 4)?;
    let method = string(args, 0)?;
    let target = string(args, 1)?;
    let body = if args.len() >= 3 { string(args, 2)?.to_string() } else { String::new() };
    let h = headers(args, 3)?;
    send(crate::text::upper(&method), &target, body, &h)
}

/// Why a request failed: a body too large is told apart.
enum Fail {
    TooLarge,
    /// The stream ended where a line was due.
    Eof,
    Other,
}

impl From<io::Error> for Fail {
    fn from(_: io::Error) -> Fail {
        Fail::Other
    }
}

struct Response {
    status: i64,
    /// Canonical name → values, in Go's sorted order.
    header: BTreeMap<String, Vec<Vec<u8>>>,
    body: Vec<u8>,
}

impl Response {
    fn first(&self, name: &str) -> Option<String> {
        self.header.get(name).and_then(|v| v.first()).map(|v| String::from_utf8_lossy(v).into_owned())
    }
}

/// One request, redirects followed; the answer as a dictionary with
/// "status", "body" and "headers" (lower-case names, values joined by ", ").
fn send(method: String, raw_url: &str, body: String, headers: &[(String, String)]) -> Result<Value> {
    if !METHODS.contains(&method.as_str()) {
        return Err(Error::new("NetworkError.HTTPBadMethod").arg(&*method));
    }
    let mut u = match Url::parse(raw_url) {
        Some(u) if (u.scheme == "http" || u.scheme == "https") && !u.hostname().is_empty() => u,
        _ => return Err(Error::new("NetworkError.HTTPBadURL").arg(raw_url)),
    };
    access()?;
    let deadline = Instant::now() + DEFAULT_TIMEOUT;
    let (mut method, mut body) = (method, body);
    let mut redirects = 0;
    let resp = loop {
        let resp = match round_trip(&method, &u, &body, headers, deadline) {
            Ok(r) => r,
            Err(Fail::TooLarge) => return Err(Error::new("NetworkError.HTTPTooLarge").arg(raw_url)),
            Err(Fail::Eof | Fail::Other) => return Err(Error::new("NetworkError.HTTPFailed").arg(raw_url)),
        };
        let location = resp.first("Location").unwrap_or_default();
        if location.is_empty() || redirects >= MAX_REDIRECTS || !matches!(resp.status, 301 | 302 | 303 | 307 | 308) {
            break resp;
        }
        let next = match u.join(&location) {
            Some(n) if n.scheme == "http" || n.scheme == "https" => n,
            _ => break resp,
        };
        u = next;
        if resp.status == 303 || (matches!(resp.status, 301 | 302) && method == "POST") {
            method = "GET".into();
            body.clear();
        }
        redirects += 1;
    };
    answer(resp)
}

/// A GET for tools (`haru install` fetching a prebuilt library): the body,
/// redirects followed, at most 32MB.
pub fn fetch(url: &str) -> std::result::Result<Vec<u8>, String> {
    let mut u = Url::parse(url).filter(|u| (u.scheme == "http" || u.scheme == "https") && !u.hostname().is_empty()).ok_or("not an http(s) address")?;
    let deadline = Instant::now() + Duration::from_secs(300);
    for _ in 0..=MAX_REDIRECTS {
        let resp = round_trip("GET", &u, "", &[], deadline).map_err(|f| match f {
            Fail::TooLarge => "too large".to_string(),
            _ => "the request failed".to_string(),
        })?;
        match resp.first("Location") {
            Some(loc) if matches!(resp.status, 301 | 302 | 303 | 307 | 308) => {
                u = u.join(&loc).ok_or("a bad redirect")?;
            }
            _ if resp.status == 200 => return Ok(resp.body),
            _ => return Err(format!("status {}", resp.status)),
        }
    }
    Err("too many redirects".into())
}

fn answer(resp: Response) -> Result<Value> {
    let headers = Dict::new();
    for (name, values) in &resp.header {
        let joined: Vec<String> = values.iter().map(|v| String::from_utf8_lossy(v).into_owned()).collect();
        headers.set(name.to_lowercase(), joined.join(", "))?;
    }
    let out = Dict::new();
    out.set("status", resp.status as f64)?;
    out.set("body", to_valid_utf8(&resp.body))?;
    out.set("headers", headers)?;
    out.into_ret()
}

/// Go's `strings.ToValidUTF8(s, "�")`: each run of bad bytes becomes
/// one replacement character.
fn to_valid_utf8(mut b: &[u8]) -> String {
    let mut out = String::with_capacity(b.len());
    while !b.is_empty() {
        match std::str::from_utf8(b) {
            Ok(s) => {
                out.push_str(s);
                break;
            }
            Err(e) => {
                let (good, rest) = b.split_at(e.valid_up_to());
                out.push_str(std::str::from_utf8(good).unwrap());
                out.push('\u{fffd}');
                b = rest;
                // Skip the whole run of bad bytes.
                loop {
                    match std::str::from_utf8(b) {
                        Err(e) if e.valid_up_to() == 0 => {
                            let n = e.error_len().unwrap_or(b.len());
                            b = &b[n..];
                        }
                        _ => break,
                    }
                }
            }
        }
    }
    out
}

enum Stream {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => match s.read(buf) {
                // A peer that closes without close_notify still ends the body.
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(0),
                r => r,
            },
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

impl Timed for Stream {
    fn set_timeouts(&self, t: Option<Duration>) -> io::Result<()> {
        match self {
            Stream::Plain(s) => s.set_timeouts(t),
            Stream::Tls(s) => s.sock.set_timeouts(t),
        }
    }
}

fn tls_config() -> Option<Arc<ClientConfig>> {
    static CONFIG: OnceLock<Option<Arc<ClientConfig>>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let verifier = rustls_platform_verifier::Verifier::new(provider.clone()).ok()?;
            let config = ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .ok()?
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(verifier))
                .with_no_client_auth();
            Some(Arc::new(config))
        })
        .clone()
}

fn connect(u: &Url, deadline: Instant) -> std::result::Result<Stream, Fail> {
    let host = String::from_utf8_lossy(u.hostname()).into_owned();
    let port = match String::from_utf8_lossy(u.port()).into_owned() {
        p if p.is_empty() => if u.scheme == "https" { "443" } else { "80" }.to_string(),
        p => p,
    };
    let left = deadline.saturating_duration_since(Instant::now());
    let tcp = dial(&join_host_port(&host, &port), Some(left.max(Duration::from_millis(1))))?;
    tcp.set_timeouts(Some(deadline.saturating_duration_since(Instant::now()).max(Duration::from_millis(1))))?;
    if u.scheme != "https" {
        return Ok(Stream::Plain(tcp));
    }
    let config = tls_config().ok_or(Fail::Other)?;
    let name = ServerName::try_from(host).map_err(|_| Fail::Other)?;
    let conn = ClientConnection::new(config, name).map_err(|_| Fail::Other)?;
    let mut tls = StreamOwned::new(conn, tcp);
    // The handshake happens here, within the deadline.
    while tls.conn.is_handshaking() {
        tls.conn.complete_io(&mut tls.sock)?;
    }
    Ok(Stream::Tls(Box::new(tls)))
}

fn round_trip(method: &str, u: &Url, body: &str, headers: &[(String, String)], deadline: Instant) -> std::result::Result<Response, Fail> {
    let stream = connect(u, deadline)?;
    let mut conn = Conn::new(stream);
    conn.set_deadline(Some(deadline));
    conn.write_all(&build_request(method, u, body, headers))?;
    read_response(&mut conn, method)
}

/// The request: used once ("close"), never compressed; a header the script
/// gives replaces the default of the same name.
fn build_request(method: &str, u: &Url, body: &str, headers: &[(String, String)]) -> Vec<u8> {
    let mut fields: BTreeMap<String, (String, Vec<u8>)> = BTreeMap::new();
    fields.insert("host".into(), ("Host".into(), u.host.clone()));
    fields.insert("user-agent".into(), ("User-Agent".into(), b"hari".to_vec()));
    fields.insert("accept-encoding".into(), ("Accept-Encoding".into(), b"identity".to_vec()));
    fields.insert("connection".into(), ("Connection".into(), b"close".to_vec()));
    if !body.is_empty() || matches!(method, "POST" | "PUT" | "PATCH") {
        fields.insert("content-length".into(), ("Content-Length".into(), body.len().to_string().into_bytes()));
    }
    for (name, value) in headers {
        fields.insert(crate::text::lower(name), (name.clone(), value.clone().into_bytes()));
    }
    let mut out = Vec::new();
    out.extend_from_slice(method.as_bytes());
    out.push(b' ');
    out.extend_from_slice(&u.request_uri());
    out.extend_from_slice(b" HTTP/1.1\r\n");
    for (name, value) in fields.values() {
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(body.as_bytes());
    out
}

type Reader = Conn<Stream>;

/// textproto's `ReadLine`: a line without its line break; an error at the end.
fn read_line(r: &mut Reader) -> std::result::Result<Vec<u8>, Fail> {
    let (mut line, ended) = r.read_until(b'\n')?;
    if ended && line.is_empty() {
        return Err(Fail::Eof);
    }
    if line.last() == Some(&b'\n') {
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
    }
    Ok(line)
}

fn trim(s: &[u8]) -> &[u8] {
    let start = s.iter().position(|&c| c != b' ' && c != b'\t').unwrap_or(s.len());
    let end = s.iter().rposition(|&c| c != b' ' && c != b'\t').map_or(start, |i| i + 1);
    &s[start..end.max(start)]
}

/// A header line with its continuation lines; empty at the blank line.
fn read_continued_line(r: &mut Reader) -> std::result::Result<Vec<u8>, Fail> {
    let line = read_line(r)?;
    if line.is_empty() {
        return Ok(line);
    }
    if !line.contains(&b':') {
        return Err(Fail::Other);
    }
    let mut buf = trim(&line).to_vec();
    loop {
        let mut spaces = 0;
        while let Some(c) = r.peek_byte() {
            if c != b' ' && c != b'\t' {
                break;
            }
            r.skip_byte();
            spaces += 1;
        }
        if spaces == 0 {
            break;
        }
        buf.push(b' ');
        match read_line(r) {
            Ok(l) => buf.extend_from_slice(trim(&l)),
            Err(_) => break,
        }
    }
    Ok(buf)
}

fn valid_field_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c)
}

/// textproto's `canonicalMIMEHeaderKey`.
fn canonical_key(k: &[u8]) -> Option<String> {
    if k.is_empty() {
        return None;
    }
    let mut no_canon = false;
    for &c in k {
        if valid_field_byte(c) {
            continue;
        }
        if c == b' ' {
            no_canon = true;
            continue;
        }
        return None;
    }
    if no_canon {
        return Some(String::from_utf8_lossy(k).into_owned());
    }
    let mut upper = true;
    let key: String = k
        .iter()
        .map(|&c| {
            let c = if upper { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() };
            upper = c == b'-';
            c as char
        })
        .collect();
    Some(key)
}

/// textproto's `ReadMIMEHeader`; the end of the stream ends it too.
fn read_header(r: &mut Reader) -> std::result::Result<BTreeMap<String, Vec<Vec<u8>>>, Fail> {
    let mut m: BTreeMap<String, Vec<Vec<u8>>> = BTreeMap::new();
    if matches!(r.peek_byte(), Some(b' ' | b'\t')) {
        return Err(Fail::Other);
    }
    loop {
        let kv = match read_continued_line(r) {
            Ok(kv) => kv,
            // The end of the stream ends the header too.
            Err(Fail::Eof) => return Ok(m),
            Err(e) => return Err(e),
        };
        if kv.is_empty() {
            return Ok(m);
        }
        let colon = kv.iter().position(|&c| c == b':').ok_or(Fail::Other)?;
        let key = canonical_key(&kv[..colon]).ok_or(Fail::Other)?;
        let v = &kv[colon + 1..];
        if v.iter().any(|&c| c < 0x20 && c != b'\t' || c == 0x7f) {
            return Err(Fail::Other);
        }
        let start = v.iter().position(|&c| c != b' ' && c != b'\t').unwrap_or(v.len());
        m.entry(key).or_default().push(v[start..].to_vec());
    }
}

fn read_response(r: &mut Reader, method: &str) -> std::result::Result<Response, Fail> {
    loop {
        let line = read_line(r)?;
        let line = String::from_utf8_lossy(&line).into_owned();
        let parts: Vec<&str> = line.splitn(3, ' ').collect();
        if parts.len() < 2 || !parts[0].starts_with("HTTP/") {
            return Err(Fail::Other);
        }
        let status = go_atoi(parts[1]).ok_or(Fail::Other)?;
        let header = read_header(r)?;
        if (100..200).contains(&status) {
            continue;
        }
        let mut resp = Response { status, header, body: Vec::new() };
        if method == "HEAD" || status == 204 || status == 304 {
            return Ok(resp);
        }
        resp.body = read_body(r, &resp)?;
        return Ok(resp);
    }
}

/// Go's `strconv.Atoi` (a sign is allowed).
fn go_atoi(s: &str) -> Option<i64> {
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Go's `strconv.ParseInt(s, base, 64)` for base 10 or 16.
fn go_parse_int(s: &str, base: u32) -> Option<i64> {
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(base)) {
        return None;
    }
    i64::from_str_radix(s, base).ok()
}

fn read_body(r: &mut Reader, resp: &Response) -> std::result::Result<Vec<u8>, Fail> {
    if resp.first("Transfer-Encoding").is_some_and(|t| t.to_lowercase().contains("chunked")) {
        return read_chunked(r);
    }
    if let Some(length) = resp.first("Content-Length").filter(|l| !l.is_empty()) {
        let n = go_parse_int(length.trim(), 10).filter(|n| *n >= 0).ok_or(Fail::Other)?;
        if n as u64 > MAX_BODY as u64 {
            return Err(Fail::TooLarge);
        }
        return Ok(r.read_exact(n as usize)?);
    }
    let body = r.read_to_end(MAX_BODY)?;
    if body.len() > MAX_BODY {
        return Err(Fail::TooLarge);
    }
    Ok(body)
}

fn read_chunked(r: &mut Reader) -> std::result::Result<Vec<u8>, Fail> {
    let mut body = Vec::new();
    loop {
        let line = read_line(r)?;
        let line = String::from_utf8_lossy(&line).into_owned();
        let size_text = line.split(';').next().unwrap_or("");
        let size = go_parse_int(size_text.trim(), 16).filter(|n| *n >= 0).ok_or(Fail::Other)?;
        if size == 0 {
            read_header(r)?;
            return Ok(body);
        }
        if body.len() as u64 + size as u64 > MAX_BODY as u64 {
            return Err(Fail::TooLarge);
        }
        body.extend(r.read_exact(size as usize)?);
        read_line(r)?;
    }
}
