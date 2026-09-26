//! [소켓] / 【ソケット】, as Hana implements it (`std/stdimpl/net.go`), and
//! what [HTTP] shares with it: the network access policy, the time limits
//! and Go's buffered reader.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use haru_sdk::prelude::*;

use crate::hana::{between, exactly, integer, number, string};

haru_sdk::entry!(pub(crate) fn entry = "socket", build);

fn build(m: &mut Module) {
    crate::describe(m, "socket", &[
        ("socket.connect", connect),
        ("socket.listen", listen),
        ("socket.accept", accept),
        ("socket.send", send),
        ("socket.receive", receive),
        ("socket.receiveline", receive_line),
        ("socket.timeout", timeout),
        ("socket.address", address),
        ("socket.close", close),
    ]);
}

static DENIED: AtomicBool = AtomicBool::new(false);

/// Turns every network operation off (`haru run --allow-net=false`).
pub fn deny_net() {
    DENIED.store(true, Ordering::Relaxed);
}

pub(crate) fn access() -> Result<()> {
    if DENIED.load(Ordering::Relaxed) {
        return Err(Error::new("NetworkError.NetBlocked"));
    }
    Ok(())
}

pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_SECONDS: f64 = 3600.0;

/// A time limit in seconds (0 to 3600).
fn seconds(args: &[Value], i: usize) -> Result<Duration> {
    let s = number(args, i)?;
    if !(0.0..=MAX_SECONDS).contains(&s) {
        return Err(Error::new("ValueError.SleepRange"));
    }
    Ok(Duration::from_nanos((s * 1e9) as u64))
}

/// Go's `net.JoinHostPort`.
pub(crate) fn join_host_port(host: &str, port: &str) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

pub(crate) fn is_timeout(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock)
}

fn net_error(e: &io::Error) -> Error {
    Error::new(if is_timeout(e) { "NetworkError.SocketTimeout" } else { "NetworkError.SocketFailed" })
}

/// A connection with a deadline for each operation, read through Go's
/// `bufio.Reader` rules (what "has arrived" means is the same).
pub(crate) struct Conn<S> {
    pub stream: S,
    buf: Box<[u8; 4096]>,
    r: usize,
    w: usize,
    err: Option<io::Error>,
    deadline: Option<Instant>,
}

/// What a connection needs from its stream besides reading and writing.
pub(crate) trait Timed {
    fn set_timeouts(&self, t: Option<Duration>) -> io::Result<()>;
}

impl Timed for TcpStream {
    fn set_timeouts(&self, t: Option<Duration>) -> io::Result<()> {
        self.set_read_timeout(t)?;
        self.set_write_timeout(t)
    }
}

impl<S: Read + Write + Timed> Conn<S> {
    pub fn new(stream: S) -> Conn<S> {
        Conn { stream, buf: Box::new([0; 4096]), r: 0, w: 0, err: None, deadline: None }
    }

    /// The operation about to run may take until then (`None`: no limit).
    pub fn set_deadline(&mut self, d: Option<Instant>) {
        self.deadline = d;
    }

    fn arm(&self) -> io::Result<()> {
        match self.deadline {
            None => self.stream.set_timeouts(None),
            Some(d) => {
                let left = d.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "deadline"));
                }
                self.stream.set_timeouts(Some(left))
            }
        }
    }

    pub fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        let mut data = data;
        while !data.is_empty() {
            self.arm()?;
            match self.stream.write(data) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => data = &data[n..],
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        self.stream.flush()
    }

    pub fn buffered(&self) -> usize {
        self.w - self.r
    }

    /// The next byte without taking it (`None` at the end or on an error,
    /// which stays for the next read).
    pub fn peek_byte(&mut self) -> Option<u8> {
        if self.r == self.w && self.err.is_none() {
            self.fill();
        }
        (self.r < self.w).then(|| self.buf[self.r])
    }

    pub fn skip_byte(&mut self) {
        if self.r < self.w {
            self.r += 1;
        }
    }

    /// One read into the buffer (Go's `fill`).
    fn fill(&mut self) {
        if self.r > 0 {
            self.buf.copy_within(self.r..self.w, 0);
            self.w -= self.r;
            self.r = 0;
        }
        if let Err(e) = self.arm() {
            self.err = Some(e);
            return;
        }
        loop {
            match self.stream.read(&mut self.buf[self.w..]) {
                Ok(0) => self.err = Some(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => self.w += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => self.err = Some(e),
            }
            return;
        }
    }

    fn take_err(&mut self) -> io::Error {
        self.err.take().unwrap_or_else(|| io::ErrorKind::UnexpectedEof.into())
    }

    /// Go's `ReadRune`: `Ok(None)` at the end of the stream.
    pub fn read_rune(&mut self) -> io::Result<Option<char>> {
        while self.r + 4 > self.w && !full_rune(&self.buf[self.r..self.w]) && self.err.is_none() && self.w - self.r < self.buf.len() {
            self.fill();
        }
        if self.r == self.w {
            let e = self.take_err();
            return if e.kind() == io::ErrorKind::UnexpectedEof { Ok(None) } else { Err(e) };
        }
        let (c, size) = decode_rune(&self.buf[self.r..self.w]);
        self.r += size;
        Ok(Some(c))
    }

    /// Go's `ReadString`/`ReadBytes`: up to and with the delimiter, or what
    /// there was when the stream ended (`Ok((bytes, true))`).
    pub fn read_until(&mut self, delim: u8) -> io::Result<(Vec<u8>, bool)> {
        let mut out = Vec::new();
        loop {
            if let Some(i) = self.buf[self.r..self.w].iter().position(|&b| b == delim) {
                out.extend_from_slice(&self.buf[self.r..self.r + i + 1]);
                self.r += i + 1;
                return Ok((out, false));
            }
            if self.err.is_some() {
                out.extend_from_slice(&self.buf[self.r..self.w]);
                self.r = self.w;
                let e = self.take_err();
                return if e.kind() == io::ErrorKind::UnexpectedEof { Ok((out, true)) } else { Err(e) };
            }
            if self.buffered() == self.buf.len() {
                out.extend_from_slice(&self.buf[..]);
                self.r = self.w;
            }
            self.fill();
        }
    }

    /// Exactly n bytes (Go's `io.ReadFull`).
    pub fn read_exact(&mut self, n: usize) -> io::Result<Vec<u8>> {
        let mut out = Vec::with_capacity(n.min(1 << 20));
        while out.len() < n {
            if self.r == self.w {
                if self.err.is_some() {
                    return Err(self.take_err());
                }
                self.fill();
                continue;
            }
            let take = (n - out.len()).min(self.w - self.r);
            out.extend_from_slice(&self.buf[self.r..self.r + take]);
            self.r += take;
        }
        Ok(out)
    }

    /// Everything up to the end, at most `limit` bytes (plus one to tell).
    pub fn read_to_end(&mut self, limit: usize) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        loop {
            out.extend_from_slice(&self.buf[self.r..self.w]);
            self.r = self.w;
            if out.len() > limit {
                out.truncate(limit + 1);
                return Ok(out);
            }
            if self.err.is_some() {
                let e = self.take_err();
                return if e.kind() == io::ErrorKind::UnexpectedEof { Ok(out) } else { Err(e) };
            }
            self.fill();
        }
    }
}

/// Go's `utf8.FullRune`.
fn full_rune(p: &[u8]) -> bool {
    let Some(&b0) = p.first() else { return false };
    let (n, lo, hi) = match b0 {
        0x00..=0x7f => return true,
        0xc2..=0xdf => (2, 0x80, 0xbf),
        0xe0 => (3, 0xa0, 0xbf),
        0xed => (3, 0x80, 0x9f),
        0xe1..=0xef => (3, 0x80, 0xbf),
        0xf0 => (4, 0x90, 0xbf),
        0xf4 => (4, 0x80, 0x8f),
        0xf1..=0xf3 => (4, 0x80, 0xbf),
        _ => return true,
    };
    if p.len() >= n {
        return true;
    }
    if p.len() > 1 && !(lo..=hi).contains(&p[1]) {
        return true;
    }
    p.len() > 2 && !(0x80..=0xbf).contains(&p[2])
}

/// Go's `utf8.DecodeRune`: U+FFFD and one byte for anything invalid.
fn decode_rune(p: &[u8]) -> (char, usize) {
    let head = &p[..p.len().min(4)];
    let valid = match std::str::from_utf8(head) {
        Ok(s) => s,
        Err(e) => std::str::from_utf8(&head[..e.valid_up_to()]).unwrap(),
    };
    match valid.chars().next() {
        Some(c) => (c, c.len_utf8()),
        None => ('\u{fffd}', 1),
    }
}

enum Socket {
    Conn { conn: Conn<TcpStream>, timeout: Duration },
    Listener { ln: TcpListener, timeout: Duration },
}

thread_local! {
    static SOCKETS: RefCell<(i64, HashMap<i64, Socket>)> = RefCell::new((1, HashMap::new()));
}

fn add(s: Socket) -> Value {
    SOCKETS.with(|t| {
        let mut t = t.borrow_mut();
        let id = t.0;
        t.0 += 1;
        t.1.insert(id, s);
        Value::num(id as f64)
    })
}

/// Runs `f` on the socket numbered by argument i.
fn with_socket<T>(args: &[Value], i: usize, f: impl FnOnce(&mut Socket) -> Result<T>) -> Result<T> {
    let id = integer(args, i)?;
    SOCKETS.with(|t| match t.borrow_mut().1.get_mut(&id) {
        Some(s) => f(s),
        None => Err(Error::new("NetworkError.SocketBadHandle").arg(id.to_string())),
    })
}

fn with_conn<T>(args: &[Value], i: usize, f: impl FnOnce(&mut Conn<TcpStream>) -> Result<T>) -> Result<T> {
    with_socket(args, i, |s| match s {
        Socket::Conn { conn, timeout } => {
            conn.set_deadline((!timeout.is_zero()).then(|| Instant::now() + *timeout));
            f(conn)
        }
        Socket::Listener { .. } => Err(Error::new("NetworkError.SocketNotConnection")),
    })
}

fn port(args: &[Value], i: usize) -> Result<u16> {
    let p = integer(args, i)?;
    if !(0..=65535).contains(&p) {
        return Err(Error::new("NetworkError.SocketPort"));
    }
    Ok(p as u16)
}

/// Connects to the first address the name has that answers in time.
pub(crate) fn dial(addr: &str, timeout: Option<Duration>) -> io::Result<TcpStream> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, "no address");
    for a in addr.to_socket_addrs()? {
        let r = match timeout {
            Some(t) if !t.is_zero() => TcpStream::connect_timeout(&a, t),
            _ => TcpStream::connect(a),
        };
        match r {
            Ok(s) => return Ok(s),
            Err(e) => last = e,
        }
    }
    Err(last)
}

fn connect(args: &[Value]) -> Result<Value> {
    between(args, 2, 3)?;
    let host = string(args, 0)?;
    let port = port(args, 1)?;
    let timeout = if args.len() == 3 { seconds(args, 2)? } else { DEFAULT_TIMEOUT };
    let addr = join_host_port(&host, &port.to_string());
    access()?;
    haru_sdk::flush_output();
    let stream = dial(&addr, Some(timeout)).map_err(|_| Error::new("NetworkError.SocketConnectFailed").arg(&*addr))?;
    Ok(add(Socket::Conn { conn: Conn::new(stream), timeout: Duration::ZERO }))
}

/// Waits for connections on a port (0: the system picks one); only this
/// computer can reach it unless a host such as "0.0.0.0" is given.
fn listen(args: &[Value]) -> Result<Value> {
    between(args, 1, 2)?;
    let port = port(args, 0)?;
    let host = if args.len() == 2 { string(args, 1)?.to_string() } else { "127.0.0.1".to_string() };
    let addr = join_host_port(&host, &port.to_string());
    access()?;
    let bind = if host.is_empty() { format!("[::]:{port}") } else { addr.clone() };
    let ln = TcpListener::bind(bind).map_err(|_| Error::new("NetworkError.SocketListenFailed").arg(&*addr))?;
    Ok(add(Socket::Listener { ln, timeout: Duration::ZERO }))
}

fn accept(args: &[Value]) -> Result<Value> {
    exactly(args, 1)?;
    haru_sdk::flush_output();
    let stream = with_socket(args, 0, |s| {
        let Socket::Listener { ln, timeout } = s else {
            return Err(Error::new("NetworkError.SocketNotListener"));
        };
        if timeout.is_zero() {
            return ln.accept().map(|(s, _)| s).map_err(|e| net_error(&e));
        }
        let deadline = Instant::now() + *timeout;
        ln.set_nonblocking(true).map_err(|e| net_error(&e))?;
        let r = loop {
            match ln.accept() {
                Ok((s, _)) => break s.set_nonblocking(false).map(|()| s),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        break Err(io::ErrorKind::TimedOut.into());
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => break Err(e),
            }
        };
        let _ = ln.set_nonblocking(false);
        r.map_err(|e| net_error(&e))
    })?;
    Ok(add(Socket::Conn { conn: Conn::new(stream), timeout: Duration::ZERO }))
}

fn send(args: &[Value]) -> Result<Value> {
    exactly(args, 2)?;
    with_conn(args, 0, |conn| {
        let text = string(args, 1)?;
        conn.write_all(text.as_bytes()).map_err(|e| net_error(&e))?;
        Ok(Value::NULL)
    })
}

/// Up to n characters of what has arrived (waiting for the first); the
/// empty text once the other side has closed.
fn receive(args: &[Value]) -> Result<Value> {
    exactly(args, 2)?;
    haru_sdk::flush_output();
    with_conn(args, 0, |conn| {
        let n = integer(args, 1)?;
        if n < 1 {
            return Err(Error::new("TypeError.NativeArgInteger").arg(2.0));
        }
        let mut out = String::new();
        for count in 0..n {
            if count > 0 && conn.buffered() == 0 {
                break;
            }
            match conn.read_rune() {
                Ok(Some(c)) => out.push(c),
                Ok(None) => break,
                Err(e) => return Err(net_error(&e)),
            }
        }
        Ok(Value::str(&out))
    })
}

/// The next line without its line break, or 비어있음 once the other side has
/// closed and nothing is left.
fn receive_line(args: &[Value]) -> Result<Value> {
    exactly(args, 1)?;
    haru_sdk::flush_output();
    with_conn(args, 0, |conn| {
        let (line, ended) = conn.read_until(b'\n').map_err(|e| net_error(&e))?;
        if ended && line.is_empty() {
            return Ok(Value::NULL);
        }
        let text = String::from_utf8_lossy(&line);
        Ok(Value::str(text.trim_end_matches(['\r', '\n'])))
    })
}

/// How long the socket's later operations may wait (0: as long as it takes).
fn timeout(args: &[Value]) -> Result<Value> {
    exactly(args, 2)?;
    with_socket(args, 0, |s| {
        let t = seconds(args, 1)?;
        match s {
            Socket::Conn { timeout, .. } | Socket::Listener { timeout, .. } => *timeout = t,
        }
        Ok(Value::NULL)
    })
}

/// Where a listener waits, or who a connection is with, as "host:port".
fn address(args: &[Value]) -> Result<Value> {
    exactly(args, 1)?;
    with_socket(args, 0, |s| {
        let a = match s {
            Socket::Listener { ln, .. } => ln.local_addr(),
            Socket::Conn { conn, .. } => conn.stream.peer_addr(),
        };
        Ok(Value::str(&a.map(|a| a.to_string()).unwrap_or_default()))
    })
}

fn close(args: &[Value]) -> Result<Value> {
    exactly(args, 1)?;
    let id = integer(args, 0)?;
    let s = SOCKETS.with(|t| t.borrow_mut().1.remove(&id));
    // Dropping it closes it.
    match s {
        None => Err(Error::new("NetworkError.SocketBadHandle").arg(id.to_string())),
        Some(_) => Ok(Value::NULL),
    }
}
