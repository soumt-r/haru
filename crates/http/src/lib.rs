//! The HTTP/1.1 plumbing Haru's server packages share (`http_server`,
//! 하늘): connections on other threads, with plain data only. A connection's
//! thread reads a request, hands it to the program's thread as a [`Job`] and
//! writes back the [`Response`] it gets, until either side ends it.
//!
//! What the packages do differently stays theirs, in [`Rules`]: how strict
//! reading is (limits, what is refused) and how an answer is written
//! (`http_server` writes exactly what Hana's Go server writes).

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::time::Duration;

/// A request as it came.
pub struct Request {
    pub method: String,
    /// The path and query as the request line has them.
    pub target: String,
    pub version: String,
    /// In order, as written (names as sent).
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// The client's address.
    pub remote: String,
}

impl Request {
    /// The first value of header `name` (any case).
    pub fn header(&self, name: &str) -> Option<&str> {
        header(&self.headers, name)
    }
}

/// What to write back.
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// A request for the program's thread, and where its answer goes.
pub struct Job {
    pub request: Request,
    pub reply: Sender<Response>,
}

/// How a package reads and writes (see the module's comment).
pub struct Rules {
    /// Longest request or header line (longer: the connection ends).
    pub max_line: Option<u64>,
    /// Most header lines (more: `refuse(431)`).
    pub max_headers: Option<usize>,
    /// Largest body.
    pub max_body: usize,
    /// A larger body is cut to `max_body` (else `refuse(413)`).
    pub truncate_body: bool,
    /// A Content-Length that is not a number is `refuse(400)` (else no body).
    pub strict_length: bool,
    /// How long a connection may say nothing.
    pub read_timeout: Option<Duration>,
    /// Whether a request line's target is one to serve (else `refuse(400)`).
    pub target_ok: fn(&str) -> bool,
    /// Writes a refusal (400, 413, 431); the connection then ends.
    pub refuse: fn(&mut TcpStream, u16) -> io::Result<()>,
    /// Writes an answer (without its body for `head`; `close`: the
    /// connection ends after it).
    pub write: fn(&mut TcpStream, &Response, head: bool, close: bool) -> io::Result<()>,
}

/// The first value of header `name` (any case) among `headers`.
pub fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

/// Accepts connections until `stop` is set (the program's thread then
/// [`wake`]s this up), each on its own thread.
pub fn accept_loop(listener: TcpListener, jobs: Sender<Job>, stop: Arc<AtomicBool>, rules: &'static Rules) {
    for conn in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        if let Ok(c) = conn {
            let jobs = jobs.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                let _ = connection(c, jobs, stop, rules);
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

/// One line without its line ending (`None` at the end of the stream);
/// bytes that are not UTF-8 become U+FFFD.
fn read_line(r: &mut BufReader<TcpStream>, max: Option<u64>) -> io::Result<Option<String>> {
    let mut line = Vec::new();
    let n = match max {
        Some(m) => r.by_ref().take(m).read_until(b'\n', &mut line)?,
        None => r.read_until(b'\n', &mut line)?,
    };
    if n == 0 {
        return Ok(None);
    }
    if max.is_some() && !line.ends_with(b"\n") {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
    }
    while matches!(line.last(), Some(b'\n' | b'\r')) {
        line.pop();
    }
    Ok(Some(String::from_utf8_lossy(&line).into_owned()))
}

/// What reading a request came to.
enum Read_ {
    Request(Request, bool),
    Refuse(u16),
    End,
}

fn read_request(r: &mut BufReader<TcpStream>, w: &mut TcpStream, rules: &Rules, remote: &str) -> io::Result<Read_> {
    // Blank lines before a request are allowed.
    let line = loop {
        match read_line(r, rules.max_line)? {
            None => return Ok(Read_::End),
            Some(l) if l.is_empty() => continue,
            Some(l) => break l,
        }
    };
    let parts: Vec<&str> = line.split(' ').collect();
    if parts.len() != 3 || parts[0].is_empty() || !parts[2].starts_with("HTTP/1.") {
        return Ok(Read_::Refuse(400));
    }
    let (method, target, version) = (parts[0].to_string(), parts[1].to_string(), parts[2].to_string());
    let mut headers: Vec<(String, String)> = Vec::new();
    loop {
        let Some(h) = read_line(r, rules.max_line)? else { return Ok(Read_::End) };
        if h.is_empty() {
            break;
        }
        if rules.max_headers.is_some_and(|m| headers.len() >= m) {
            return Ok(Read_::Refuse(431));
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    if !(rules.target_ok)(&target) {
        return Ok(Read_::Refuse(400));
    }
    if header(&headers, "Expect").is_some_and(|e| e.eq_ignore_ascii_case("100-continue")) {
        w.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
    }
    let chunked = header(&headers, "Transfer-Encoding").is_some_and(|t| t.to_ascii_lowercase().contains("chunked"));
    let body = if chunked {
        match read_chunked(r, rules)? {
            Some(b) => b,
            None => return Ok(Read_::Refuse(413)),
        }
    } else {
        let n: usize = match header(&headers, "Content-Length").map(|l| l.parse()) {
            None => 0,
            Some(Ok(n)) => n,
            Some(Err(_)) if rules.strict_length => return Ok(Read_::Refuse(400)),
            Some(Err(_)) => 0,
        };
        if n > rules.max_body && !rules.truncate_body {
            return Ok(Read_::Refuse(413));
        }
        let mut b = vec![0; n];
        r.read_exact(&mut b)?;
        b.truncate(rules.max_body);
        b
    };
    let close = header(&headers, "Connection").is_some_and(|c| c.eq_ignore_ascii_case("close"))
        || (version == "HTTP/1.0" && !header(&headers, "Connection").is_some_and(|c| c.eq_ignore_ascii_case("keep-alive")));
    Ok(Read_::Request(Request { method, target, version, headers, body, remote: remote.to_string() }, close))
}

/// A chunked body (`None`: too large, and not to be cut).
fn read_chunked(r: &mut BufReader<TcpStream>, rules: &Rules) -> io::Result<Option<Vec<u8>>> {
    let mut body = Vec::new();
    loop {
        let line = read_line(r, rules.max_line)?.ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        let size_text = line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
        if size == 0 {
            // Trailers, up to the blank line.
            while let Some(t) = read_line(r, rules.max_line)? {
                if t.is_empty() {
                    break;
                }
            }
            return Ok(Some(body));
        }
        if body.len() + size > rules.max_body && !rules.truncate_body {
            return Ok(None);
        }
        let mut chunk = vec![0; size];
        r.read_exact(&mut chunk)?;
        let room = rules.max_body.saturating_sub(body.len());
        body.extend_from_slice(&chunk[..size.min(room)]);
        read_line(r, rules.max_line)?;
    }
}

/// One connection: requests in, answers out, until either side stops.
fn connection(stream: TcpStream, jobs: Sender<Job>, stop: Arc<AtomicBool>, rules: &'static Rules) -> io::Result<()> {
    stream.set_read_timeout(rules.read_timeout)?;
    let remote = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    loop {
        let (request, close) = match read_request(&mut reader, &mut writer, rules, &remote)? {
            Read_::Request(r, close) => (r, close),
            Read_::Refuse(status) => return (rules.refuse)(&mut writer, status),
            Read_::End => return Ok(()),
        };
        let head = request.method == "HEAD";
        let (reply, answer) = channel();
        if stop.load(Ordering::SeqCst) || jobs.send(Job { request, reply }).is_err() {
            return Ok(());
        }
        let Ok(a) = answer.recv() else { return Ok(()) };
        (rules.write)(&mut writer, &a, head, close)?;
        if close {
            return Ok(());
        }
    }
}

/// Whether an answer of `status` has no body (1xx, 204, 304).
pub fn bodiless(status: u16) -> bool {
    (100..200).contains(&status) || status == 204 || status == 304
}

/// Seconds since 1970.
pub fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// A date as HTTP writes it: `Sun, 06 Nov 1994 08:49:37 GMT`.
pub fn http_date(secs: i64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
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
    format!(
        "{}, {d:02} {} {y:04} {:02}:{:02}:{:02} GMT",
        DAYS[days.rem_euclid(7) as usize],
        MONTHS[(m - 1) as usize],
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

/// Whether `k` can be a header name (a header can never break the answer's lines).
pub fn header_name_ok(k: &str) -> bool {
    !k.is_empty() && k.bytes().all(|c| c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c))
}
