//! The part of Go's `net/url` that [HTTP] uses, step for step: `Parse`,
//! `Hostname`, `Port`, `RequestURI` and resolving a redirect's `Location`.
//! Unescaped parts are bytes, as in Go (a %-escape may make invalid UTF-8).

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Path,
    Host,
    Zone,
    UserPassword,
    Fragment,
}

#[derive(Clone, Default)]
pub struct Url {
    pub scheme: String,
    opaque: Vec<u8>,
    has_user: bool,
    /// Host and port, unescaped.
    pub host: Vec<u8>,
    path: Vec<u8>,
    raw_path: Vec<u8>,
    force_query: bool,
    raw_query: Vec<u8>,
    fragment: Vec<u8>,
}

fn is_hex(c: u8) -> bool {
    c.is_ascii_hexdigit()
}

fn unhex(c: u8) -> u8 {
    (c as char).to_digit(16).unwrap_or(0) as u8
}

fn should_escape(c: u8, mode: Mode) -> bool {
    if c.is_ascii_alphanumeric() {
        return false;
    }
    if (mode == Mode::Host || mode == Mode::Zone)
        && matches!(c, b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';' | b'=' | b':' | b'[' | b']' | b'<' | b'>' | b'"')
    {
        return false;
    }
    match c {
        b'-' | b'_' | b'.' | b'~' => return false,
        b'$' | b'&' | b'+' | b',' | b'/' | b':' | b';' | b'=' | b'?' | b'@' => match mode {
            Mode::Path => return c == b'?',
            Mode::UserPassword => return matches!(c, b'@' | b'/' | b'?' | b':'),
            Mode::Fragment => return false,
            _ => {}
        },
        _ => {}
    }
    if mode == Mode::Fragment && matches!(c, b'!' | b'(' | b')' | b'*') {
        return false;
    }
    true
}

fn unescape(s: &[u8], mode: Mode) -> Option<Vec<u8>> {
    let mut i = 0;
    while i < s.len() {
        match s[i] {
            b'%' => {
                if i + 2 >= s.len() || !is_hex(s[i + 1]) || !is_hex(s[i + 2]) {
                    return None;
                }
                if mode == Mode::Host && unhex(s[i + 1]) < 8 && &s[i..i + 3] != b"%25" {
                    return None;
                }
                if mode == Mode::Zone {
                    let v = unhex(s[i + 1]) << 4 | unhex(s[i + 2]);
                    if &s[i..i + 3] != b"%25" && v != b' ' && should_escape(v, Mode::Host) {
                        return None;
                    }
                }
                i += 3;
            }
            c => {
                if (mode == Mode::Host || mode == Mode::Zone) && c < 0x80 && should_escape(c, mode) {
                    return None;
                }
                i += 1;
            }
        }
    }
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' {
            out.push(unhex(s[i + 1]) << 4 | unhex(s[i + 2]));
            i += 3;
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    Some(out)
}

fn escape(s: &[u8], mode: Mode) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for &c in s {
        if should_escape(c, mode) {
            out.extend_from_slice(format!("%{c:02X}").as_bytes());
        } else {
            out.push(c);
        }
    }
    out
}

fn valid_encoded(s: &[u8], mode: Mode) -> bool {
    s.iter().all(|&c| {
        matches!(c, b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';' | b'=' | b':' | b'@' | b'[' | b']' | b'%')
            || !should_escape(c, mode)
    })
}

fn valid_optional_port(port: &[u8]) -> bool {
    match port.split_first() {
        None => true,
        Some((b':', digits)) => digits.iter().all(u8::is_ascii_digit),
        _ => false,
    }
}

fn find(s: &[u8], b: u8) -> Option<usize> {
    s.iter().position(|&c| c == b)
}

fn rfind(s: &[u8], b: u8) -> Option<usize> {
    s.iter().rposition(|&c| c == b)
}

fn find_sub(s: &[u8], sub: &[u8]) -> Option<usize> {
    s.windows(sub.len()).position(|w| w == sub)
}

fn parse_host(host: &[u8]) -> Option<Vec<u8>> {
    if host.starts_with(b"[") {
        let i = rfind(host, b']')?;
        if !valid_optional_port(&host[i + 1..]) {
            return None;
        }
        if let Some(zone) = find_sub(&host[..i], b"%25") {
            let mut out = unescape(&host[..zone], Mode::Host)?;
            out.extend(unescape(&host[zone..i], Mode::Zone)?);
            out.extend(unescape(&host[i..], Mode::Host)?);
            return Some(out);
        }
    } else if let Some(i) = rfind(host, b':') {
        if !valid_optional_port(&host[i..]) {
            return None;
        }
    }
    unescape(host, Mode::Host)
}

fn valid_userinfo(s: &[u8]) -> bool {
    // Go looks at characters; every allowed one is ASCII.
    s.iter().all(|&c| {
        c.is_ascii_alphanumeric()
            || matches!(c, b'-' | b'.' | b'_' | b':' | b'~' | b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';' | b'=' | b'%' | b'@')
    })
}

/// (has user, host).
fn parse_authority(authority: &[u8]) -> Option<(bool, Vec<u8>)> {
    let at = rfind(authority, b'@');
    let host = parse_host(match at {
        None => authority,
        Some(i) => &authority[i + 1..],
    })?;
    let Some(i) = at else { return Some((false, host)) };
    let userinfo = &authority[..i];
    if !valid_userinfo(userinfo) {
        return None;
    }
    match find(userinfo, b':') {
        None => {
            unescape(userinfo, Mode::UserPassword)?;
        }
        Some(c) => {
            unescape(&userinfo[..c], Mode::UserPassword)?;
            unescape(&userinfo[c + 1..], Mode::UserPassword)?;
        }
    }
    Some((true, host))
}

fn get_scheme(raw: &[u8]) -> Option<(&[u8], &[u8])> {
    for (i, &c) in raw.iter().enumerate() {
        match c {
            b'a'..=b'z' | b'A'..=b'Z' => {}
            b'0'..=b'9' | b'+' | b'-' | b'.' if i != 0 => {}
            b':' if i == 0 => return None,
            b':' => return Some((&raw[..i], &raw[i + 1..])),
            _ => return Some((b"", raw)),
        }
    }
    Some((b"", raw))
}

impl Url {
    /// Go's `url.Parse`.
    pub fn parse(raw: &str) -> Option<Url> {
        let raw = raw.as_bytes();
        let (u, frag) = match find(raw, b'#') {
            Some(i) => (&raw[..i], Some(&raw[i + 1..])),
            None => (raw, None),
        };
        let mut url = parse(u)?;
        if let Some(f) = frag.filter(|f| !f.is_empty()) {
            url.fragment = unescape(f, Mode::Fragment)?;
        }
        Some(url)
    }

    fn set_path(&mut self, p: &[u8]) -> Option<()> {
        let path = unescape(p, Mode::Path)?;
        self.raw_path = if escape(&path, Mode::Path) == p { Vec::new() } else { p.to_vec() };
        self.path = path;
        Some(())
    }

    fn escaped_path(&self) -> Vec<u8> {
        if !self.raw_path.is_empty() && valid_encoded(&self.raw_path, Mode::Path) {
            if unescape(&self.raw_path, Mode::Path).as_deref() == Some(&self.path[..]) {
                return self.raw_path.clone();
            }
        }
        if self.path == b"*" {
            return b"*".to_vec();
        }
        escape(&self.path, Mode::Path)
    }

    fn split_host_port(&self) -> (&[u8], &[u8]) {
        let mut host = &self.host[..];
        let mut port: &[u8] = b"";
        if let Some(c) = rfind(host, b':') {
            if valid_optional_port(&host[c..]) {
                port = &host[c + 1..];
                host = &host[..c];
            }
        }
        if host.len() >= 2 && host.starts_with(b"[") && host.ends_with(b"]") {
            host = &host[1..host.len() - 1];
        }
        (host, port)
    }

    pub fn hostname(&self) -> &[u8] {
        self.split_host_port().0
    }

    pub fn port(&self) -> &[u8] {
        self.split_host_port().1
    }

    pub fn request_uri(&self) -> Vec<u8> {
        let mut result = self.opaque.clone();
        if result.is_empty() {
            result = self.escaped_path();
            if result.is_empty() {
                result = b"/".to_vec();
            }
        } else if result.starts_with(b"//") {
            result = [self.scheme.as_bytes(), b":", &result].concat();
        }
        if self.force_query || !self.raw_query.is_empty() {
            result.push(b'?');
            result.extend_from_slice(&self.raw_query);
        }
        result
    }

    /// Go's `u.Parse(ref)`: a reference resolved against this URL.
    pub fn join(&self, reference: &str) -> Option<Url> {
        let r = Url::parse(reference)?;
        let mut url = r.clone();
        if r.scheme.is_empty() {
            url.scheme = self.scheme.clone();
        }
        if !r.scheme.is_empty() || !r.host.is_empty() || r.has_user {
            url.set_path(&resolve_path(&r.escaped_path(), b""))?;
            return Some(url);
        }
        if !r.opaque.is_empty() {
            url.has_user = false;
            url.host.clear();
            url.path.clear();
            return Some(url);
        }
        if r.path.is_empty() && !r.force_query && r.raw_query.is_empty() {
            url.raw_query = self.raw_query.clone();
            if r.fragment.is_empty() {
                url.fragment = self.fragment.clone();
            }
        }
        if r.path.is_empty() && !self.opaque.is_empty() {
            url.opaque = self.opaque.clone();
            url.has_user = false;
            url.host.clear();
            url.path.clear();
            return Some(url);
        }
        url.host = self.host.clone();
        url.has_user = self.has_user;
        url.set_path(&resolve_path(&self.escaped_path(), &r.escaped_path()))?;
        Some(url)
    }
}

fn parse(raw: &[u8]) -> Option<Url> {
    if raw.iter().any(|&b| b < b' ' || b == 0x7f) {
        return None;
    }
    let mut url = Url::default();
    if raw == b"*" {
        url.path = b"*".to_vec();
        return Some(url);
    }
    let (scheme, mut rest) = get_scheme(raw)?;
    url.scheme = String::from_utf8_lossy(scheme).to_lowercase();
    let questions = rest.iter().filter(|&&c| c == b'?').count();
    if rest.ends_with(b"?") && questions == 1 {
        url.force_query = true;
        rest = &rest[..rest.len() - 1];
    } else if let Some(i) = find(rest, b'?') {
        url.raw_query = rest[i + 1..].to_vec();
        rest = &rest[..i];
    }
    if !rest.starts_with(b"/") {
        if !url.scheme.is_empty() {
            url.opaque = rest.to_vec();
            return Some(url);
        }
        let segment = &rest[..find(rest, b'/').unwrap_or(rest.len())];
        if find(segment, b':').is_some() {
            return None;
        }
    }
    let mut path = rest;
    if (!url.scheme.is_empty() || !rest.starts_with(b"///")) && rest.starts_with(b"//") {
        let authority_and_rest = &rest[2..];
        let (authority, tail) = match find(authority_and_rest, b'/') {
            Some(i) => (&authority_and_rest[..i], &authority_and_rest[i..]),
            None => (authority_and_rest, &b""[..]),
        };
        let (user, host) = parse_authority(authority)?;
        url.has_user = user;
        url.host = host;
        path = tail;
    }
    url.set_path(path)?;
    Some(url)
}

/// Go's `resolvePath` (RFC 3986 dot segments).
fn resolve_path(base: &[u8], reference: &[u8]) -> Vec<u8> {
    let full: Vec<u8> = if reference.is_empty() {
        base.to_vec()
    } else if reference[0] != b'/' {
        let i = rfind(base, b'/').map_or(0, |i| i + 1);
        [&base[..i], reference].concat()
    } else {
        reference.to_vec()
    };
    if full.is_empty() {
        return Vec::new();
    }
    let mut dst: Vec<u8> = vec![b'/'];
    let mut first = true;
    let mut elem: &[u8] = b"";
    let mut remaining = &full[..];
    let mut found = true;
    while found {
        match find(remaining, b'/') {
            Some(i) => {
                elem = &remaining[..i];
                remaining = &remaining[i + 1..];
            }
            None => {
                elem = remaining;
                remaining = b"";
                found = false;
            }
        }
        if elem == b"." {
            first = false;
            continue;
        }
        if elem == b".." {
            let s = dst[1..].to_vec();
            dst.clear();
            dst.push(b'/');
            match rfind(&s, b'/') {
                None => first = true,
                Some(index) => dst.extend_from_slice(&s[..index]),
            }
        } else {
            if !first {
                dst.push(b'/');
            }
            dst.extend_from_slice(elem);
            first = false;
        }
    }
    if elem == b"." || elem == b".." {
        dst.push(b'/');
    }
    if dst.len() > 1 && dst[1] == b'/' {
        dst.remove(0);
    }
    dst
}
