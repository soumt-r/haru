//! Runtime errors carry a code and arguments, never text. Text is made at the
//! edge, in the language of the program (the approach of Hana's `errs`,
//! extended to modules: a module ships its own templates).

use haru_abi::kind;

use crate::modules::Runtime;
use crate::value::Value;

#[derive(Debug, Clone)]
pub struct RuntimeError {
    /// The module that raised it, or `None` for the runtime's own errors.
    pub module: Option<usize>,
    pub code: String,
    pub args: Vec<Value>,
}

impl RuntimeError {
    pub fn core(code: &str) -> RuntimeError {
        RuntimeError { module: None, code: code.to_string(), args: Vec::new() }
    }

    pub fn arg(mut self, v: Value) -> RuntimeError {
        self.args.push(v);
        self
    }

    /// The full code: `math.NotNumber` for a module, `ArgumentType` for the runtime.
    pub fn qualified_code(&self, rt: &Runtime) -> String {
        match self.module {
            Some(m) => format!("{}.{}", rt.module_id(m), self.code),
            None => self.code.clone(),
        }
    }

    /// The message in `lang` ("hari", "kanade"); English when nothing better exists.
    pub fn message(&self, rt: &Runtime, lang: &str) -> String {
        let template = self
            .module
            .and_then(|m| rt.message(m, &self.code, lang).or_else(|| rt.message(m, &self.code, "en")))
            .or_else(|| core_message(&self.code, lang));
        let args: Vec<String> = if self.module.is_none() && self.code == "ArgumentType" {
            // {1} is a parameter kind: name it in the program's language.
            self.args
                .iter()
                .enumerate()
                .map(|(i, a)| match (i, a.as_num()) {
                    (1, Some(k)) => kind_name(k as u32, lang).to_string(),
                    _ => a.to_string(),
                })
                .collect()
        } else {
            self.args.iter().map(|a| a.to_string()).collect()
        };
        match template {
            Some(t) => fill(t, &args),
            None => {
                let mut s = self.qualified_code(rt);
                if !args.is_empty() {
                    s.push_str(": ");
                    s.push_str(&args.join(", "));
                }
                s
            }
        }
    }
}

fn fill(template: &str, args: &[String]) -> String {
    let mut out = template.to_string();
    for (i, a) in args.iter().enumerate() {
        out = out.replace(&format!("{{{i}}}"), a);
    }
    out
}

fn core_message(code: &str, lang: &str) -> Option<&'static str> {
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
