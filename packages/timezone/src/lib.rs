//! The native half of the timezone package: the IANA time zone database
//! (jiff's copy), behind the `<네이티브_Offset>` ... functions its Hari and
//! Kanade entry points wrap. It answers exactly as Hana's Go library does,
//! messages included; Hana hands that library its arguments as JSON, so the
//! same conversions happen here (비어있음 reads as 0 or "", NaN and objects
//! cannot be sent at all).

use std::collections::HashSet;
use std::sync::OnceLock;

use haru_sdk::abi::tag;
use haru_sdk::prelude::*;
use jiff::tz::TimeZone;
use jiff::Timestamp;

fn build(m: &mut Module) {
    m.raw("Offset", |a| call("Offset", a, 2, offset));
    m.raw("Format", |a| call("Format", a, 3, format));
    m.raw("Parse", |a| call("Parse", a, 3, parse));
    m.raw("Weekday", |a| call("Weekday", a, 2, weekday));
}

haru_sdk::export!("timezone", build);

/// A failure the way the Go library reports one: Hana makes it
/// `NativeCallFailed(function, message)`.
struct Fail(String);

type Out = std::result::Result<Value, Fail>;

fn call(name: &str, args: &[Value], n: usize, f: fn(&[Value]) -> Out) -> Result<Value> {
    // Hana sends the arguments as JSON first; what JSON cannot hold fails there.
    for a in args {
        json_check(a, 0)?;
    }
    if args.len() != n {
        let what = match name {
            "Offset" => "(time, zone)",
            "Weekday" => "(time, zone)",
            "Format" => "(time, format, zone)",
            _ => "(text, format, zone)",
        };
        return Err(native_error(name, format!("{name} needs {what}")));
    }
    f(args).map_err(|Fail(m)| native_error(name, m))
}

fn native_error(name: &str, message: String) -> Error {
    Error::new("ImportError.NativeCallFailed").arg(name).arg(message)
}

/// Hana's `ToJSON` of an argument (function values travel as ids).
fn json_check(v: &Value, depth: usize) -> Result<()> {
    let unsupported = || Error::new("ValueError.JSONUnsupported");
    match v.tag() {
        tag::NULL | tag::BOOL | tag::STR | tag::FUNC => Ok(()),
        tag::NUM if v.as_num().unwrap().is_finite() => Ok(()),
        tag::LIST if depth < 1000 => v.as_list().unwrap().iter().try_for_each(|x| json_check(&x, depth + 1)),
        tag::DICT => {
            let d = v.as_dict().unwrap();
            for k in d.keys().iter() {
                if k.as_str().is_none() {
                    return Err(unsupported());
                }
                json_check(&d.get(&k).unwrap_or(Value::NULL), depth + 1)?;
            }
            Ok(())
        }
        _ => Err(unsupported()),
    }
}

/// A time: Unix seconds (JSON null reads as 0), floored.
fn time_arg(v: &Value) -> std::result::Result<i64, Fail> {
    let bad = || Fail("the time must be a number of seconds".into());
    let s = match v.tag() {
        tag::NULL => 0.0,
        tag::NUM => v.as_num().unwrap(),
        _ => return Err(bad()),
    };
    if !s.is_finite() || s.abs() > 8.64e12 {
        return Err(bad());
    }
    Ok(s.floor() as i64)
}

fn string_arg(v: &Value, what: &str) -> std::result::Result<String, Fail> {
    match v.tag() {
        tag::NULL => Ok(String::new()),
        tag::STR => Ok(v.as_str().unwrap().to_string()),
        _ => Err(Fail(format!("{what} must be text"))),
    }
}

/// Go's `%q`.
fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{7}' => out.push_str("\\a"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{b}' => out.push_str("\\v"),
            c if printable(c) => out.push(c),
            c if (c as u32) < 0x80 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (c as u32) < 0x10000 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push_str(&format!("\\U{:08x}", c as u32)),
        }
    }
    out.push('"');
    out
}

/// Close to Go's `strconv.IsPrint`: no controls, no spaces but ' ', no
/// format characters.
fn printable(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    let n = c as u32;
    !(c.is_control()
        || c.is_whitespace()
        || n == 0xad
        || (0x200b..=0x200f).contains(&n)
        || (0x202a..=0x202e).contains(&n)
        || (0x2060..=0x206f).contains(&n)
        || n == 0xfeff
        || (0xfff9..=0xfffb).contains(&n)
        || (0xe000..=0xf8ff).contains(&n)
        || n >= 0xf0000)
}

enum Zone {
    Fixed(i64),
    Named(TimeZone),
}

/// Names the database has, exactly as written (Go's lookup is case-sensitive).
fn names() -> &'static HashSet<String> {
    static NAMES: OnceLock<HashSet<String>> = OnceLock::new();
    NAMES.get_or_init(|| jiff::tz::db().available().map(|n| n.as_str().to_string()).collect())
}

/// An IANA name, "UTC", or a fixed offset ("+09:00", "-0530"); never the
/// computer's own zone.
fn zone(name: &str) -> std::result::Result<Zone, Fail> {
    let unknown = || Fail(format!("unknown time zone {}", quote(name)));
    match name {
        "UTC" | "Z" => return Ok(Zone::Fixed(0)),
        "" | "Local" => return Err(unknown()),
        _ => {}
    }
    let b = name.as_bytes();
    let fixed = match b.len() {
        6 if b[3] == b':' => Some((b[0], &b[1..3], &b[4..6])),
        5 => Some((b[0], &b[1..3], &b[3..5])),
        _ => None,
    };
    if let Some((sign, h, m)) = fixed {
        if (sign == b'+' || sign == b'-') && h.iter().chain(m).all(u8::is_ascii_digit) {
            let hours = ((h[0] - b'0') * 10 + h[1] - b'0') as i64;
            let minutes = ((m[0] - b'0') * 10 + m[1] - b'0') as i64;
            if hours > 23 || minutes > 59 {
                return Err(unknown());
            }
            let s = hours * 3600 + minutes * 60;
            return Ok(Zone::Fixed(if sign == b'-' { -s } else { s }));
        }
    }
    if !names().contains(name) {
        return Err(unknown());
    }
    TimeZone::get(name).map(Zone::Named).map_err(|_| unknown())
}

/// 400 Gregorian years: the calendar and the zones' yearly rules repeat.
const ERA: i64 = 12_622_780_800;

/// The zone's offset from UTC (seconds, east positive) at a Unix time.
fn offset_at(z: &Zone, unix: i64) -> i64 {
    match z {
        Zone::Fixed(s) => *s,
        Zone::Named(tz) => {
            // Beyond the years jiff holds, the same moment of an equal year.
            let (min, max) = (Timestamp::MIN.as_second() + 86400, Timestamp::MAX.as_second() - 86400);
            let mut t = unix;
            if t > max {
                t -= ((t - max) / ERA + 1) * ERA;
            } else if t < min {
                t += ((min - t) / ERA + 1) * ERA;
            }
            let ts = Timestamp::from_second(t).expect("in range");
            tz.to_offset(ts).seconds() as i64
        }
    }
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}

/// (year, month, day, hour, minute, second, weekday with 0 for Sunday) of a
/// clock reading given as seconds since 1970 on that clock.
fn clock(local: i64) -> [i64; 7] {
    let days = local.div_euclid(86400);
    let secs = local.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    [y, m, d, secs / 3600, secs / 60 % 60, secs % 60, (days + 4).rem_euclid(7)]
}

const TOKENS: [&str; 6] = ["YYYY", "MM", "DD", "HH", "mm", "ss"];

fn token_at(layout: &[u8], i: usize) -> Option<usize> {
    TOKENS.iter().position(|t| layout[i..].starts_with(t.as_bytes()))
}

fn pad(n: i64, width: usize) -> String {
    let s = n.to_string();
    format!("{}{s}", "0".repeat(width.saturating_sub(s.len())))
}

fn offset(a: &[Value]) -> Out {
    let t = time_arg(&a[0])?;
    let name = string_arg(&a[1], "the zone")?;
    let z = zone(&name)?;
    Ok(Value::num(offset_at(&z, t) as f64))
}

fn format(a: &[Value]) -> Out {
    let t = time_arg(&a[0])?;
    let layout = string_arg(&a[1], "the format")?;
    let name = string_arg(&a[2], "the zone")?;
    let z = zone(&name)?;
    let c = clock(t + offset_at(&z, t));
    let lb = layout.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lb.len() {
        match token_at(lb, i) {
            None => {
                out.push(lb[i]);
                i += 1;
            }
            Some(k) => {
                out.extend_from_slice(pad(c[k], if k == 0 { 4 } else { 2 }).as_bytes());
                i += TOKENS[k].len();
            }
        }
    }
    Ok(Value::str(&String::from_utf8_lossy(&out)))
}

/// A clock reading back to Unix seconds: a date that does not exist, or a
/// reading skipped by a clock change, fails; one that happens twice is the
/// earlier.
fn parse(a: &[Value]) -> Out {
    let text = string_arg(&a[0], "the text")?;
    let layout = string_arg(&a[1], "the format")?;
    let name = string_arg(&a[2], "the zone")?;
    let z = zone(&name)?;
    let invalid = format!("{} is not a date written as {}", quote(&text), quote(&layout));
    let (tb, lb) = (text.as_bytes(), layout.as_bytes());
    let mut parts = [1970i64, 1, 1, 0, 0, 0];
    let (mut pos, mut i) = (0, 0);
    while i < lb.len() {
        let Some(k) = token_at(lb, i) else {
            if pos >= tb.len() || tb[pos] != lb[i] {
                return Err(Fail(invalid));
            }
            pos += 1;
            i += 1;
            continue;
        };
        let len = TOKENS[k].len();
        if pos + len > tb.len() || !tb[pos..pos + len].iter().all(u8::is_ascii_digit) {
            return Err(Fail(invalid));
        }
        parts[k] = tb[pos..pos + len].iter().fold(0, |n, d| n * 10 + (d - b'0') as i64);
        pos += len;
        i += len;
    }
    if pos != tb.len() {
        return Err(Fail(invalid));
    }
    // The reading as if it were UTC, checked so February 30 does not move on.
    let [y, mo, d, h, mi, s] = parts;
    let (ny, nmo) = (y + (mo - 1).div_euclid(12), (mo - 1).rem_euclid(12) + 1);
    let local = days_from_civil(ny, nmo, 1) * 86400 + (d - 1) * 86400 + h * 3600 + mi * 60 + s;
    if clock(local)[..6] != parts {
        return Err(Fail(invalid));
    }
    // The offsets a day before and after bracket any clock change near it.
    let mut best: Option<i64> = None;
    for probe in [local - 86400, local + 86400] {
        let off = offset_at(&z, probe);
        let instant = local - off;
        if offset_at(&z, instant) == off && best.is_none_or(|b| instant < b) {
            best = Some(instant);
        }
    }
    match best {
        Some(b) => Ok(Value::num(b as f64)),
        None => Err(Fail(format!("{invalid} (that clock time does not exist in {name})"))),
    }
}

/// 1 for Monday through 7 for Sunday, on the zone's calendar.
fn weekday(a: &[Value]) -> Out {
    let t = time_arg(&a[0])?;
    let name = string_arg(&a[1], "the zone")?;
    let z = zone(&name)?;
    let day = clock(t + offset_at(&z, t))[6];
    Ok(Value::num(if day == 0 { 7.0 } else { day as f64 }))
}
