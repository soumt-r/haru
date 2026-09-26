//! Runtime errors carry a code and arguments, never text. Text is made at the
//! edge, in the language of the program. The runtime's own codes and wording
//! are Hana's (`catalog.rs`, generated from hana/errs); a module ships its own
//! templates.

use haru_abi::{kind, tag};

use crate::catalog::{CATALOG, RUNTIME_LABEL};
use crate::format::{go_i64, go_v_float, number};
use crate::lang::{Lang, HARI, KANADE};
use crate::modules::Runtime;
use crate::value::Value;

#[derive(Debug, Clone)]
pub struct RuntimeError {
    /// The module that raised it, or `None` for the runtime's own errors.
    pub module: Option<usize>,
    /// A Hana code (`TypeError.OperandTypeMismatch`), a module's code, or
    /// one of the signals `break`/`return` that escaped to the top.
    pub code: String,
    pub args: Vec<Value>,
}

impl RuntimeError {
    pub fn core(code: &str) -> RuntimeError {
        RuntimeError { module: None, code: code.to_string(), args: Vec::new() }
    }

    /// `발생시키자`: a value the program threw.
    pub fn thrown(v: Value) -> RuntimeError {
        RuntimeError { module: None, code: THROWN.to_string(), args: vec![v] }
    }

    pub fn thrown_value(&self) -> Option<&Value> {
        (self.module.is_none() && self.code == THROWN).then(|| &self.args[0])
    }

    pub fn arg(mut self, v: Value) -> RuntimeError {
        self.args.push(v);
        self
    }

    pub fn str_arg(self, s: &str) -> RuntimeError {
        self.arg(Value::str(s))
    }

    pub fn num_arg(self, n: f64) -> RuntimeError {
        self.arg(Value::num(n))
    }

    /// The full code: `math.NotNumber` for a module, the code itself otherwise.
    pub fn qualified_code(&self, rt: &Runtime) -> String {
        match self.module {
            Some(m) => format!("{}.{}", rt.module_id(m), self.code),
            None => self.code.clone(),
        }
    }

    /// The message as a caught error's text or the CLI's report shows it.
    pub fn message(&self, rt: &Runtime, lang: &str) -> String {
        self.localize(Some(rt), lang_named(lang))
    }

    pub fn localize(&self, rt: Option<&Runtime>, lang: &Lang) -> String {
        // Hana's ThrownError.Error(): an object's 메시지 (or メッセージ), else Go's %v.
        if let Some(v) = self.thrown_value() {
            if let Some(o) = v.as_object() {
                let props = o.props.borrow();
                for key in ["메시지", "メッセージ"] {
                    if let Some(s) = props.get(&crate::symbol::intern(key)).and_then(|m| m.as_str()) {
                        return s.to_string();
                    }
                }
            }
            return go_v(v);
        }
        if let Some(m) = self.module {
            let rt = rt.expect("a module error needs its runtime");
            let template = rt
                .message(m, &self.code, lang.name)
                .or_else(|| rt.message(m, &self.code, "en"))
                .or_else(|| legacy_message(&self.code, lang.name));
            let args: Vec<String> = self.args.iter().map(|a| crate::format::display(a, lang)).collect();
            return match template {
                Some(t) => fill(t, &args),
                None => format!("{}.{}", rt.module_id(m), self.code),
            };
        }
        if let Ok(i) = CATALOG.binary_search_by(|e| e.0.cmp(self.code.as_str())) {
            let entry = haru_wording(self.code.as_str()).unwrap_or(CATALOG[i]);
            let template = match lang.locale {
                1 => entry.2,
                2 => entry.3,
                _ => entry.1,
            };
            let kind = self.code.split('.').next().unwrap_or("");
            return format!("{kind}: {}", go_format(template, &self.args, lang));
        }
        if let Some(t) = legacy_message(&self.code, lang.name) {
            let args: Vec<String> = self
                .args
                .iter()
                .enumerate()
                .map(|(i, a)| match (self.code.as_str(), i, a.as_num()) {
                    ("ArgumentType", 1, Some(k)) => kind_name(k as u32, lang.name).to_string(),
                    _ => crate::format::display(a, lang),
                })
                .collect();
            return fill(t, &args);
        }
        // `break`, `return`: what Hana's CLI prints for them.
        self.code.clone()
    }

    /// The line the CLI prints for an uncaught error.
    pub fn report(&self, rt: Option<&Runtime>, lang: &Lang) -> String {
        let label = match lang.locale {
            1 => RUNTIME_LABEL.1,
            2 => RUNTIME_LABEL.2,
            _ => RUNTIME_LABEL.0,
        };
        format!("{label}: {}", self.localize(rt, lang))
    }
}

fn lang_named(name: &str) -> &'static Lang {
    match name {
        "kanade" => &KANADE,
        _ => &HARI,
    }
}

/// Marks a thrown value (not a Hana code: it never reaches a catalog).
const THROWN: &str = "Thrown";

/// Go's `%v` of a value as Hana holds it (for thrown values that are not
/// error objects).
pub fn go_v(v: &Value) -> String {
    match v.tag() {
        tag::NULL => "<nil>".to_string(),
        tag::BOOL => v.as_bool().unwrap().to_string(),
        tag::NUM => go_v_float(v.as_num().unwrap()),
        tag::STR => v.as_str().unwrap().to_string(),
        tag::LIST => {
            let items: Vec<String> = v.as_list().unwrap().items.borrow().iter().map(go_v).collect();
            format!("&{{[{}]}}", items.join(" "))
        }
        tag::DICT => {
            let mut items: Vec<String> =
                v.as_dict().unwrap().map.borrow().iter().map(|(k, e)| format!("{}:{}", go_v(&k.0), go_v(e))).collect();
            items.sort();
            format!("map[{}]", items.join(" "))
        }
        tag::OBJECT => format!("&{{{} map[] <nil>}}", crate::symbol::name(v.as_object().unwrap().class)),
        crate::value::CLASS => format!("&{{{}}}", crate::symbol::name(v.as_class().unwrap())),
        _ => "?".to_string(),
    }
}

/// Renders a Go format template: `%s`, `%d`, `%v`, `%[n]s` and `%%`.
pub fn go_format(template: &str, args: &[Value], lang: &Lang) -> String {
    let mut out = String::new();
    let mut next = 0;
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let mut index = None;
        if chars.peek() == Some(&'[') {
            chars.next();
            let mut n = String::new();
            for d in chars.by_ref() {
                if d == ']' {
                    break;
                }
                n.push(d);
            }
            index = n.parse::<usize>().ok().map(|n| n - 1);
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some(verb) => {
                let i = index.unwrap_or(next);
                next = i + 1;
                match args.get(i) {
                    Some(a) => out.push_str(&go_verb(verb, a, lang)),
                    None => out.push_str(&format!("%!{verb}(MISSING)")),
                }
            }
            None => out.push('%'),
        }
    }
    out
}

fn go_verb(verb: char, v: &Value, lang: &Lang) -> String {
    match (verb, v.tag()) {
        ('d', tag::NUM) => go_i64(v.as_num().unwrap()).to_string(),
        ('v', tag::NUM) => go_v_float(v.as_num().unwrap()),
        ('v', tag::BOOL) => v.as_bool().unwrap().to_string(),
        ('v', tag::NULL) => "<nil>".to_string(),
        (_, tag::STR) => v.as_str().unwrap().to_string(),
        (_, tag::NUM) => number(v.as_num().unwrap()),
        _ => crate::format::display(v, lang),
    }
}

fn fill(template: &str, args: &[String]) -> String {
    let mut out = template.to_string();
    for (i, a) in args.iter().enumerate() {
        out = out.replace(&format!("{{{i}}}"), a);
    }
    out
}

/// Messages of the native-call checks (M0), in the module-template form.
fn legacy_message(code: &str, lang: &str) -> Option<&'static str> {
    Some(match (code, lang) {
        ("ArgumentCount", "hari") => "인자가 {0}개 필요한데 {1}개가 들어왔어요.",
        ("ArgumentCount", "kanade") => "引数は{0}個必要ですが、{1}個渡されました。",
        ("ArgumentCount", _) => "expected {0} arguments, got {1}",
        ("ArgumentType", "hari") => "{0}번째 인자는 [{1}]이어야 해요.",
        ("ArgumentType", "kanade") => "{0}番目の引数は【{1}】でなければなりません。",
        ("ArgumentType", _) => "argument {0} must be {1}",
        ("NotCallable", "hari") => "함수가 아닌 값은 부를 수 없어요.",
        ("NotCallable", "kanade") => "関数ではない値は呼び出せません。",
        ("NotCallable", _) => "value is not callable",
        ("Panic", "hari") => "네이티브 모듈 안에서 문제가 생겼어요.",
        ("Panic", "kanade") => "ネイティブモジュールの中で問題が起きました。",
        ("Panic", _) => "native module panicked",
        ("NativeFailed", "hari") => "네이티브 함수가 이유 없이 실패했어요.",
        ("NativeFailed", "kanade") => "ネイティブ関数が理由なく失敗しました。",
        ("NativeFailed", _) => "native function failed without an error",
        _ => return None,
    })
}

fn kind_name(k: u32, lang: &str) -> &'static str {
    let kanade = lang == "kanade";
    match k {
        kind::NUM => if kanade { "数" } else { "숫자" },
        kind::STR => if kanade { "文字列" } else { "문자열" },
        kind::BOOL => if kanade { "論理" } else { "논리" },
        kind::LIST => if kanade { "リスト" } else { "목록" },
        kind::DICT => if kanade { "辞書" } else { "사전" },
        kind::FUNC => if kanade { "関数" } else { "함수" },
        _ => if kanade { "何でも" } else { "아무거나" },
    }
}

/// Hana's messages that name Hana's own tools, as Haru words them (the
/// packages are Haru's: `haru install`, not `hana install`).
fn haru_wording(code: &str) -> Option<(&'static str, &'static str, &'static str, &'static str)> {
    match code {
        "ImportError.ImportPackageNotInstalled" => Some((
            "ImportError.ImportPackageNotInstalled",
            "Package '[%s]' is not installed. 'haru install' downloads it.",
            "패키지 '[%s]'가 설치되어 있지 않아요. 'haru install'로 내려받을 수 있어요.",
            "パッケージ『[%s]』がインストールされていません。「haru install」でダウンロードできます。",
        )),
        _ => None,
    }
}
