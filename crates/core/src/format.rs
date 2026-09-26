//! Values as text, exactly as Hana prints them (`FormatValue`), plus the Go
//! number formats error messages use.

use haru_abi::tag;

use crate::lang::Lang;
use crate::value::Value;

thread_local! {
    static FUNCTION_TEXT: std::cell::RefCell<Vec<std::rc::Rc<str>>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// What each function of the running program shows when printed (by index).
pub fn set_function_texts(texts: Vec<std::rc::Rc<str>>) {
    FUNCTION_TEXT.with(|t| *t.borrow_mut() = texts);
}

/// How deep a list may nest in its printed form (a list can contain itself).
const MAX_DEPTH: usize = 100;

/// What `출력하자` prints for a value.
pub fn display(v: &Value, lang: &Lang) -> String {
    let mut out = String::new();
    write_value(&mut out, v, lang, 0);
    out
}

pub fn write_value(out: &mut String, v: &Value, lang: &Lang, depth: usize) {
    match v.tag() {
        tag::NULL => out.push_str(lang.null),
        tag::BOOL => out.push_str(if v.as_bool().unwrap() { lang.true_word } else { lang.false_word }),
        tag::NUM => out.push_str(&number(v.as_num().unwrap())),
        tag::STR => out.push_str(v.as_str().unwrap()),
        tag::LIST => {
            if depth > MAX_DEPTH {
                out.push_str("[...]");
                return;
            }
            out.push('[');
            for (i, item) in v.as_list().unwrap().items.borrow().iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_value(out, item, lang, depth + 1);
            }
            out.push(']');
        }
        tag::DICT => {
            // Go maps have no order: entries are sorted by their text.
            let mut entries: Vec<String> = v
                .as_dict()
                .unwrap()
                .map
                .borrow()
                .iter()
                .map(|(k, val)| {
                    let mut e = String::new();
                    write_value(&mut e, &k.0, lang, depth + 1);
                    e.push_str(": ");
                    write_value(&mut e, val, lang, depth + 1);
                    e
                })
                .collect();
            entries.sort();
            out.push('{');
            out.push_str(&entries.join(", "));
            out.push('}');
        }
        tag::FUNC => match v.as_func() {
            Some(&crate::value::FuncObj::User(p)) => {
                FUNCTION_TEXT.with(|t| out.push_str(t.borrow().get(p as usize).map_or("<함수>", |s| s)))
            }
            _ => out.push_str("<함수>"),
        },
        tag::OBJECT => {
            let name = crate::symbol::name(v.as_object().unwrap().class);
            out.push_str(lang.object_format.0);
            out.push_str(name);
            out.push_str(lang.object_format.1);
        }
        // Haru's own (Hana has no resources): shown like an object of its kind.
        tag::RESOURCE => {
            out.push_str(lang.object_format.0);
            out.push_str(v.as_resource().unwrap().name(lang.name));
            out.push_str(lang.object_format.1);
        }
        // Go's `%v` of Hana's *ClassReference.
        crate::value::CLASS => {
            out.push_str("&{");
            out.push_str(crate::symbol::name(v.as_class().unwrap()));
            out.push('}');
        }
        _ => out.push('?'),
    }
}

/// A number the way Hana prints it: Go's `FormatFloat(v, 'f', -1, 64)`, the
/// shortest exact decimal without an exponent.
pub fn number(n: f64) -> String {
    if n.is_nan() {
        "NaN".to_string()
    } else if n.is_infinite() {
        if n > 0.0 { "+Inf" } else { "-Inf" }.to_string()
    } else if n == 0.0 {
        if n.is_sign_negative() { "-0" } else { "0" }.to_string()
    } else {
        format!("{n}")
    }
}

/// Go's `%v` for a float64 (shortest `%g`): exponent form when the decimal
/// exponent is below -4 or at least 6 (`1e+06`, `1e-05`).
pub fn go_v_float(n: f64) -> String {
    if !n.is_finite() || n == 0.0 {
        return number(n);
    }
    // Shortest round-trip digits in scientific form: "1.2345e6".
    let sci = format!("{n:e}");
    let (mantissa, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    if !(-4..6).contains(&exp) {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{mantissa}e{sign}{:02}", exp.abs())
    } else {
        format!("{n}")
    }
}

/// Go's `int64(f)` on amd64: out-of-range and NaN become the minimum.
pub fn go_i64(f: f64) -> i64 {
    // 2^63: the first float64 outside int64.
    const LIMIT: f64 = 9_223_372_036_854_775_808.0;
    if !(-LIMIT..LIMIT).contains(&f) {
        i64::MIN
    } else {
        f as i64
    }
}

/// Go's `int(f)` used for indexes (the same conversion on 64-bit).
pub fn go_int(f: f64) -> i64 {
    go_i64(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_print_like_go() {
        assert_eq!(number(3.0), "3");
        assert_eq!(number(0.1 + 0.2), "0.30000000000000004");
        assert_eq!(number(1e21), "1000000000000000000000");
        assert_eq!(number(-0.0), "-0");
        assert_eq!(number(f64::INFINITY), "+Inf");
        assert_eq!(go_v_float(100000.0), "100000");
        assert_eq!(go_v_float(1e6), "1e+06");
        assert_eq!(go_v_float(123456789.0), "1.23456789e+08");
        assert_eq!(go_v_float(0.0001), "0.0001");
        assert_eq!(go_v_float(1e-5), "1e-05");
        assert_eq!(go_i64(1e20), i64::MIN);
        assert_eq!(go_i64(-3.7), -3);
    }
}
