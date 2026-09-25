//! `haru` — only module inspection for now; `run` arrives with the VM (M2).
//!
//!   haru version
//!   haru modules [--lang hari|kanade]
//!   haru call [--lang hari|kanade] <모듈> <함수> [인자...]
//!   haru ast <file>          (syntax tree as JSON)
//!   haru ast-check <dir>     (compare with Hana's trees from tools/astdump)

mod ast;

use std::path::Path;
use std::process::ExitCode;

use haru_core::{Runtime, Value};

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let lang = take_flag(&mut args, "--lang").unwrap_or_else(|| "hari".to_string());

    let mut rt = Runtime::new();
    for entry in haru_std::MODULES {
        if let Err(e) = rt.load_static(*entry) {
            eprintln!("haru: {e}");
            return ExitCode::FAILURE;
        }
    }

    match args.first().map(String::as_str) {
        Some("version") => println!("haru {}", env!("CARGO_PKG_VERSION")),
        Some("modules") => list_modules(&rt, &lang),
        Some("call") if args.len() >= 3 => return call(&rt, &lang, &args[1], &args[2], &args[3..]),
        Some("ast") if args.len() == 2 => return ast::print(Path::new(&args[1])),
        Some("ast-check") if args.len() == 2 => return ast::check(Path::new(&args[1])),
        _ => {
            eprintln!("usage: haru version | modules [--lang L] | call [--lang L] <module> <function> [args...] | ast <file> | ast-check <dir>");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> Option<String> {
    let i = args.iter().position(|a| a == flag)?;
    args.remove(i);
    (i < args.len()).then(|| args.remove(i))
}

fn name_in<'a>(names: &'a [(String, String)], lang: &str) -> &'a str {
    names.iter().find(|(l, _)| l == lang).map_or("-", |(_, n)| n.as_str())
}

fn list_modules(rt: &Runtime, lang: &str) {
    for m in rt.describe() {
        println!("{} ({})", name_in(&m.names, lang), m.id);
        for f in &m.functions {
            println!("  {} ({}, {})", name_in(&f.names, lang), f.id, f.params);
        }
    }
}

/// Arguments that read as numbers are numbers; everything else is a string.
fn call(rt: &Runtime, lang: &str, module: &str, func: &str, raw: &[String]) -> ExitCode {
    let Some(m) = rt.module(lang, module) else {
        eprintln!("haru: no module {module}");
        return ExitCode::FAILURE;
    };
    let Some(f) = rt.function(m, lang, func) else {
        eprintln!("haru: no function {func} in {module}");
        return ExitCode::FAILURE;
    };
    let args: Vec<Value> = raw.iter().map(|a| a.parse().map_or_else(|_| Value::str(a), Value::num)).collect();
    match rt.call(f, &args) {
        Ok(v) => {
            println!("{v}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{}", e.message(rt, lang));
            ExitCode::FAILURE
        }
    }
}
