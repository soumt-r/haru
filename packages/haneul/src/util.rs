//! Small pieces of HTTP: URL and cookie text, HTML escaping, dates, file
//! types, and the signing of session cookies.

use sha2::{Digest, Sha256};

/// `%XX` decoding (`plus`: `+` is a space, as in a query or a form). Bytes
/// that are not UTF-8 become U+FFFD; a broken `%` stays as it is.
pub fn percent_decode(s: &str, plus: bool) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() && hex(b[i + 1]).is_some() && hex(b[i + 2]).is_some() => {
                out.push(hex(b[i + 1]).unwrap() * 16 + hex(b[i + 2]).unwrap());
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
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    (c as char).to_digit(16).map(|d| d as u8)
}

/// `%XX` encoding of everything but unreserved characters.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &c in s.as_bytes() {
        if c.is_ascii_alphanumeric() || b"-._~".contains(&c) {
            out.push(c as char);
        } else {
            out.push_str(&format!("%{c:02X}"));
        }
    }
    out
}

/// The pairs of a query or a form, in order (a name may come more than once).
pub fn parse_query(s: &str) -> Vec<(String, String)> {
    s.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (percent_decode(k, true), percent_decode(v, true))
        })
        .collect()
}

pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&#34;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// The cookies of a `Cookie` header (values `%`-decoded).
pub fn parse_cookies(header: &str) -> Vec<(String, String)> {
    header
        .split(';')
        .filter_map(|p| {
            let (k, v) = p.split_once('=')?;
            let v = v.trim().trim_matches('"');
            Some((k.trim().to_string(), percent_decode(v, false)))
        })
        .filter(|(k, _)| !k.is_empty())
        .collect()
}

/// Whether a cookie value can go as it is.
fn cookie_safe(c: u8) -> bool {
    c.is_ascii_graphic() && !b"\",;\\%".contains(&c)
}

/// A `Set-Cookie` value.
pub struct CookieOptions {
    pub max_age: Option<i64>,
    pub expires: Option<String>,
    pub path: String,
    pub domain: Option<String>,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: Option<String>,
}

impl Default for CookieOptions {
    fn default() -> CookieOptions {
        CookieOptions { max_age: None, expires: None, path: "/".into(), domain: None, secure: false, http_only: false, same_site: None }
    }
}

pub fn set_cookie(name: &str, value: &str, o: &CookieOptions) -> String {
    let mut out = format!("{name}=");
    for &c in value.as_bytes() {
        if cookie_safe(c) {
            out.push(c as char);
        } else {
            out.push_str(&format!("%{c:02X}"));
        }
    }
    if let Some(d) = &o.domain {
        out.push_str(&format!("; Domain={d}"));
    }
    if let Some(e) = &o.expires {
        out.push_str(&format!("; Expires={e}"));
    }
    if let Some(m) = o.max_age {
        out.push_str(&format!("; Max-Age={m}"));
    }
    if o.secure {
        out.push_str("; Secure");
    }
    if o.http_only {
        out.push_str("; HttpOnly");
    }
    out.push_str(&format!("; Path={}", o.path));
    if let Some(s) = &o.same_site {
        out.push_str(&format!("; SameSite={s}"));
    }
    out
}

/// Year, month, day of a day counted from 1970-01-01.
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

pub fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// `Sun, 06 Nov 1994 08:49:37 GMT`.
pub fn http_date(secs: i64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (y, m, d) = civil(days);
    format!(
        "{}, {d:02} {} {y:04} {:02}:{:02}:{:02} GMT",
        DAYS[days.rem_euclid(7) as usize],
        MONTHS[(m - 1) as usize],
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

/// `27/Sep/2026 08:49:37` (UTC), for the request log.
pub fn log_date(secs: i64) -> String {
    let rem = secs.rem_euclid(86400);
    let (y, m, d) = civil(secs.div_euclid(86400));
    format!("{d:02}/{}/{y:04} {:02}:{:02}:{:02}", MONTHS[(m - 1) as usize], rem / 3600, rem / 60 % 60, rem % 60)
}

/// The `Content-Type` of a file by its extension.
pub fn mime_for(path: &std::path::Path) -> &'static str {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "txt" | "hr" | "knd" | "md" => "text/plain; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "avif" => "image/avif",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "wasm" => "application/wasm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    }
}

pub fn status_text(code: u16) -> &'static str {
    match code {
        100 => "Continue",
        101 => "Switching Protocols",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        203 => "Non-Authoritative Information",
        204 => "No Content",
        205 => "Reset Content",
        206 => "Partial Content",
        300 => "Multiple Choices",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        402 => "Payment Required",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        408 => "Request Timeout",
        409 => "Conflict",
        410 => "Gone",
        411 => "Length Required",
        412 => "Precondition Failed",
        413 => "Content Too Large",
        414 => "URI Too Long",
        415 => "Unsupported Media Type",
        416 => "Range Not Satisfiable",
        417 => "Expectation Failed",
        418 => "I'm a teapot",
        422 => "Unprocessable Content",
        423 => "Locked",
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
        _ => "",
    }
}

// ---------------------------------------------------------------------------
// Signing

pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let inner = Sha256::new().chain_update(ipad).chain_update(msg).finalize();
    Sha256::new().chain_update(opad).chain_update(inner).finalize().into()
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Base64 with the URL alphabet and no padding.
pub fn b64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len() * 4 / 3 + 3);
    for chunk in data.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut n, mut bits) = (0u32, 0);
    for c in s.bytes() {
        let v = B64.iter().position(|&x| x == c)? as u32;
        n = n << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((n >> bits) as u8);
            n &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// `payload.signature` for a text.
pub fn sign(key: &[u8], text: &str) -> String {
    let payload = b64_encode(text.as_bytes());
    let mac = hmac_sha256(key, payload.as_bytes());
    format!("{payload}.{}", b64_encode(&mac))
}

/// The text of a signed value when its signature is right.
pub fn unsign(key: &[u8], signed: &str) -> Option<String> {
    let (payload, mac) = signed.rsplit_once('.')?;
    let want = hmac_sha256(key, payload.as_bytes());
    let got = b64_decode(mac)?;
    // Compared in full, whatever differs, so timing tells nothing.
    if got.len() != want.len() || got.iter().zip(want.iter()).fold(0u8, |d, (a, b)| d | (a ^ b)) != 0 {
        return None;
    }
    String::from_utf8(b64_decode(payload)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc_4231() {
        let mac = hmac_sha256(&[0x0b; 20], b"Hi There");
        let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7");
    }

    #[test]
    fn base64_round_trips() {
        for s in ["", "a", "ab", "abc", "abcd", "하늘"] {
            assert_eq!(b64_decode(&b64_encode(s.as_bytes())).unwrap(), s.as_bytes());
        }
        assert_eq!(b64_encode(b"hello"), "aGVsbG8");
    }

    #[test]
    fn signed_text_needs_its_key() {
        let s = sign(b"key", "{\"a\":1}");
        assert_eq!(unsign(b"key", &s).as_deref(), Some("{\"a\":1}"));
        assert_eq!(unsign(b"other", &s), None);
        let tampered = s.replacen('e', "f", 1);
        assert_eq!(unsign(b"key", &tampered), None);
    }

    #[test]
    fn urls_and_dates() {
        assert_eq!(percent_decode("%ED%95%98%EB%8A%98+x%2", true), "하늘 x%2");
        assert_eq!(parse_query("a=1&b=%EA%B0%80&a=2&c"), vec![
            ("a".to_string(), "1".to_string()),
            ("b".to_string(), "가".to_string()),
            ("a".to_string(), "2".to_string()),
            ("c".to_string(), String::new())
        ]);
        assert_eq!(http_date(784111777), "Sun, 06 Nov 1994 08:49:37 GMT");
        assert_eq!(log_date(784111777), "06/Nov/1994 08:49:37");
    }
}
