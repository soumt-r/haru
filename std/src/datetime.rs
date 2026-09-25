//! [날짜] / 【日時】, as Hana implements it (`std/stdimpl/datetime.go`).
//! Dates are Unix seconds in the computer's time zone; formats use YYYY MM
//! DD HH mm ss. Go's calendar reaches far past chrono's, so the calendar is
//! computed here and chrono only gives the zone's offset.

use chrono::{Local, Offset, TimeZone};
use haru_sdk::prelude::*;

use crate::hana::{exactly, number, string};

haru_sdk::entry!(pub(crate) fn entry = "datetime", build);

fn build(m: &mut Module) {
    crate::describe(m, "datetime", &[
        ("datetime.now", now),
        ("datetime.format", format),
        ("datetime.parse", parse),
        ("datetime.weekday", weekday),
        ("datetime.sleep", sleep),
    ]);
}

const TOKENS: [&str; 6] = ["YYYY", "MM", "DD", "HH", "mm", "ss"];

fn token_at(layout: &str, i: usize) -> Option<&'static str> {
    TOKENS.iter().copied().find(|t| layout.as_bytes()[i..].starts_with(t.as_bytes()))
}

/// Days since 1970-01-01 of a date of the proleptic Gregorian calendar.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// (year, month, day) of a day counted from 1970-01-01.
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

/// The local zone's offset from UTC (seconds) at a Unix time. Far from today
/// the offset of the nearest year chrono knows is used.
fn offset_at(unix: i64) -> i64 {
    // 1900..2200: the zone's rules are about today's world anyway.
    let clamped = unix.clamp(-2_208_988_800, 7_258_118_400);
    match Local.timestamp_opt(clamped, 0) {
        chrono::LocalResult::Single(t) | chrono::LocalResult::Ambiguous(t, _) => t.offset().fix().local_minus_utc() as i64,
        chrono::LocalResult::None => 0,
    }
}

struct Civil {
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
    /// 0 for Sunday.
    weekday: i64,
}

fn local(unix: i64) -> Civil {
    let t = unix + offset_at(unix);
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400);
    let (year, month, day) = civil_from_days(days);
    Civil { year, month, day, hour: secs / 3600, minute: secs / 60 % 60, second: secs % 60, weekday: (days + 4).rem_euclid(7) }
}

/// Seconds as a time Go can hold (NativeArgNumber otherwise).
fn seconds(s: f64) -> Result<i64> {
    if !s.is_finite() || s.abs() > 8.64e12 {
        return Err(Error::new("TypeError.NativeArgNumber").arg(1.0));
    }
    Ok(s.floor() as i64)
}

fn now(args: &[Value]) -> Result<Value> {
    exactly(args, 0)?;
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    Ok(Value::num(ms as f64 / 1000.0))
}

/// Go's `strconv.Itoa` padded with zeros in front (before a minus sign too).
fn pad(n: i64, width: usize) -> String {
    let s = n.to_string();
    format!("{}{s}", "0".repeat(width.saturating_sub(s.len())))
}

fn format(args: &[Value]) -> Result<Value> {
    exactly(args, 2)?;
    let s = number(args, 0)?;
    let layout = string(args, 1)?;
    let unix = seconds(s)?;
    let t = local(unix);
    let mut out = Vec::new();
    let mut i = 0;
    while i < layout.len() {
        let Some(token) = token_at(&layout, i) else {
            out.push(layout.as_bytes()[i]);
            i += 1;
            continue;
        };
        let text = match token {
            "YYYY" => pad(t.year, 4),
            "MM" => pad(t.month, 2),
            "DD" => pad(t.day, 2),
            "HH" => pad(t.hour, 2),
            "mm" => pad(t.minute, 2),
            _ => pad(t.second, 2),
        };
        out.extend_from_slice(text.as_bytes());
        i += token.len();
    }
    Ok(Value::str(&String::from_utf8(out).unwrap_or_default()))
}

/// Text in the format back to Unix seconds; parts the format leaves out are
/// 1970-01-01 00:00:00, and a date or time that does not exist is an error.
fn parse(args: &[Value]) -> Result<Value> {
    exactly(args, 2)?;
    let text = string(args, 0)?;
    let layout = string(args, 1)?;
    let invalid = || Error::new("ValueError.DateInvalid").arg(&*text);
    let (tb, lb) = (text.as_bytes(), layout.as_bytes());
    let mut parts = [1970i64, 1, 1, 0, 0, 0];
    let mut pos = 0;
    let mut i = 0;
    while i < lb.len() {
        let Some(token) = token_at(&layout, i) else {
            if pos >= tb.len() || tb[pos] != lb[i] {
                return Err(invalid());
            }
            pos += 1;
            i += 1;
            continue;
        };
        if pos + token.len() > tb.len() {
            return Err(invalid());
        }
        let digits = &tb[pos..pos + token.len()];
        if !digits.iter().all(u8::is_ascii_digit) {
            return Err(invalid());
        }
        let n = digits.iter().fold(0i64, |n, d| n * 10 + (d - b'0') as i64);
        parts[TOKENS.iter().position(|t| *t == token).unwrap()] = n;
        pos += token.len();
        i += token.len();
    }
    if pos != tb.len() {
        return Err(invalid());
    }
    let [y, mo, d, h, mi, s] = parts;
    // Go's time.Date normalizes; a part that changes by it did not exist.
    let (ny, nmo) = (y + (mo - 1).div_euclid(12), (mo - 1).rem_euclid(12) + 1);
    let wall = days_from_civil(ny, nmo, 1) * 86400 + (d - 1) * 86400 + h * 3600 + mi * 60 + s;
    let guess = wall - offset_at(wall);
    let unix = wall - offset_at(guess);
    let t = local(unix);
    if (t.year, t.month, t.day, t.hour, t.minute, t.second) != (y, mo, d, h, mi, s) {
        return Err(invalid());
    }
    Ok(Value::num(unix as f64))
}

/// 1 for Monday through 7 for Sunday.
fn weekday(args: &[Value]) -> Result<Value> {
    exactly(args, 1)?;
    let day = local(seconds(number(args, 0)?)?).weekday;
    Ok(Value::num(if day == 0 { 7.0 } else { day as f64 }))
}

/// Waits that many seconds (0 to 3600).
fn sleep(args: &[Value]) -> Result<Value> {
    exactly(args, 1)?;
    let s = number(args, 0)?;
    if s.is_nan() || !(0.0..=3600.0).contains(&s) {
        return Err(Error::new("ValueError.SleepRange"));
    }
    // Go's time.Duration is whole nanoseconds.
    std::thread::sleep(std::time::Duration::from_nanos((s * 1e9) as u64));
    Ok(Value::NULL)
}
