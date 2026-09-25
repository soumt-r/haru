//! [CSV] / 【CSV】, as Hana implements it (`std/stdimpl/csv.go`): rows of
//! text cells (RFC 4180); reading never turns a cell into a number.

use haru_sdk::abi::tag;
use haru_sdk::prelude::*;

use crate::hana::{between, list, new_list, string};

haru_sdk::entry!(pub(crate) fn entry = "csv", build);

fn build(m: &mut Module) {
    crate::describe(m, "csv", &[("csv.parse", parse), ("csv.stringify", stringify)]);
}

/// The optional delimiter: one character, not a quote or a line break.
fn delimiter(args: &[Value], i: usize) -> Result<char> {
    if i >= args.len() {
        return Ok(',');
    }
    let s = string(args, i)?;
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if !matches!(c, '"' | '\n' | '\r' | '\u{fffd}') => Ok(c),
        _ => Err(Error::new("ValueError.CSVDelimiter")),
    }
}

fn parse(args: &[Value]) -> Result<Value> {
    between(args, 1, 2)?;
    let text = string(args, 0)?;
    let delim = delimiter(args, 1)?;
    let text: &str = &text;
    read(text.strip_prefix('\u{feff}').unwrap_or(text), delim)
}

/// Row by row: a line break inside quotes belongs to the cell, a quote inside
/// a cell that did not start with one is an ordinary character, anything but
/// a delimiter or a line break after a closing quote is an error. A blank
/// line is a row of one empty cell; a final line break starts no row.
fn read(text: &str, delim: char) -> Result<Value> {
    let chars: Vec<char> = text.chars().collect();
    let mut rows: Vec<Value> = Vec::new();
    let mut row: Vec<Value> = Vec::new();
    let mut field = String::new();
    let (mut started, mut quoted, mut in_quotes, mut after_quote) = (false, false, false, false);

    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_quotes {
            if c != '"' {
                field.push(c);
            } else if chars.get(i + 1) == Some(&'"') {
                field.push('"');
                i += 1;
            } else {
                in_quotes = false;
                after_quote = true;
            }
        } else if c == delim {
            row.push(Value::str(&std::mem::take(&mut field)));
            (started, quoted, after_quote) = (false, false, false);
        } else if c == '\n' || c == '\r' {
            if c == '\r' && chars.get(i + 1) == Some(&'\n') {
                i += 1;
            }
            row.push(Value::str(&std::mem::take(&mut field)));
            (started, quoted, after_quote) = (false, false, false);
            rows.push(new_list(std::mem::take(&mut row))?);
        } else if after_quote {
            return Err(Error::new("ValueError.CSVInvalid"));
        } else if c == '"' && !started {
            (in_quotes, quoted, started) = (true, true, true);
        } else {
            field.push(c);
            started = true;
        }
        i += 1;
    }
    if in_quotes {
        return Err(Error::new("ValueError.CSVInvalid"));
    }
    if started || quoted || !row.is_empty() {
        row.push(Value::str(&field));
        rows.push(new_list(row)?);
    }
    new_list(rows)
}

/// Rows joined by line breaks (none after the last). A cell is quoted only
/// when it must be: it holds the delimiter, a quote or a line break, or it is
/// the empty only cell of its row.
fn stringify(args: &[Value]) -> Result<Value> {
    between(args, 1, 2)?;
    let rows = list(args, 0)?;
    let delim = delimiter(args, 1)?;
    let mut out = String::new();
    for (n, r) in rows.iter().enumerate() {
        let row = match r.as_list() {
            Some(row) if !row.is_empty() => row,
            _ => return Err(Error::new("ValueError.CSVUnsupported")),
        };
        if n > 0 {
            out.push('\n');
        }
        let only = row.len() == 1;
        for (k, v) in row.iter().enumerate() {
            if k > 0 {
                out.push(delim);
            }
            let text = cell(&v)?;
            if text.contains(['"', '\r', '\n', delim]) || (text.is_empty() && only) {
                out.push('"');
                out.push_str(&text.replace('"', "\"\""));
                out.push('"');
            } else {
                out.push_str(&text);
            }
        }
    }
    Ok(Value::str(&out))
}

/// A cell's text: text as it is, a number in JSON's form, 비어있음 as nothing.
fn cell(v: &Value) -> Result<String> {
    match v.tag() {
        tag::STR => Ok(v.as_str().unwrap().to_string()),
        tag::NULL => Ok(String::new()),
        tag::NUM => {
            let x = v.as_num().unwrap();
            if !x.is_finite() {
                return Err(Error::new("ValueError.CSVUnsupported"));
            }
            Ok(crate::json::number(x))
        }
        _ => Err(Error::new("ValueError.CSVUnsupported")),
    }
}
