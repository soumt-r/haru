//! The `haru` command, as a library: `haru build` makes binaries that are
//! this command plus packages linked in (and maybe a program to run).
//!
//!   haru run [--time] [--allow-file=false] [--allow-net=false] <file>
//!   haru init | add <package>[@version] [--allow-build] | install | remove <package> | list
//!   haru build [--with <package>]... [-o <file>] [<program>]
//!   haru version
//!   haru modules [--lang hari|kanade]
//!   haru call [--lang hari|kanade] <모듈> <함수> [인자...]
//!   haru run [--time] <file>
//!   haru dis <file>          (compiled code)
//!   haru ast <file>          (syntax tree as JSON)
//!   haru ast-check <dir>     (compare with Hana's trees from tools/astdump)

mod ast;
mod build;
mod manifest;
pub mod packages;
mod pkgtool;
mod run;
mod semver;

pub use packages::Bundled;

/// What a binary made by `haru build` carries.
#[derive(Default)]
pub struct Build {
    /// Packages linked in, found before any other of the same name.
    pub packages: Vec<Bundled>,
    /// A program to run instead of being the `haru` command.
    pub program: Option<Embedded>,
}

/// A program inside the binary: its main file and the files it imports.
pub struct Embedded {
    pub main: &'static str,
    pub files: &'static [(&'static str, &'static str)],
}

use std::path::Path;
use std::process::ExitCode;

use haru_core::{Runtime, Value};

/// The `haru` command (with what a build added).
pub fn main_with(build: Build) -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let lang = take_flag(&mut args, "--lang").unwrap_or_else(|| "hari".to_string());
    let time = match args.iter().position(|a| a == "--time") {
        Some(i) => {
            args.remove(i);
            true
        }
        None => false,
    };
    // Hana's `--allow-file=false` / `--allow-net=false` (both allowed by default).
    for (flag, deny) in [("--allow-file", haru_std::deny_files as fn()), ("--allow-net", haru_std::deny_net)] {
        match bool_flag(&mut args, flag) {
            Some(Ok(false)) => deny(),
            Some(Ok(true)) | None => {}
            Some(Err(v)) => {
                eprintln!("haru: invalid argument \"{v}\" for \"{flag}\" flag");
                return ExitCode::FAILURE;
            }
        }
    }

    if let Some(program) = build.program {
        return run::run(Path::new(program.main), time, build.packages, program.files);
    }

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
        Some("run") if args.len() == 2 => return run::run(Path::new(&args[1]), time, build.packages, &[]),
        Some("build") => return build::command(&args[1..]),
        Some(c @ ("init" | "add" | "install" | "remove" | "list")) => return pkgtool::command(c, &args[1..]),
        Some("dis") if args.len() == 2 => return dis(Path::new(&args[1])),
        Some("ast") if args.len() == 2 => return ast::print(Path::new(&args[1])),
        Some("ast-check") if args.len() == 2 => return ast::check(Path::new(&args[1])),
        _ => {
            eprintln!("usage: haru run <file> | init | add <package>[@version] | install | remove <package> | list | version | modules [--lang L] | call [--lang L] <module> <function> [args...] | ast <file> | ast-check <dir>");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

/// A boolean flag the way Go's flag packages read one: `--flag` or
/// `--flag=<strconv.ParseBool>`.
fn bool_flag(args: &mut Vec<String>, flag: &str) -> Option<Result<bool, String>> {
    let i = args.iter().position(|a| a == flag || a.starts_with(&format!("{flag}=")))?;
    let arg = args.remove(i);
    Some(match arg.split_once('=').map(|(_, v)| v) {
        None => Ok(true),
        Some("1" | "t" | "T" | "TRUE" | "true" | "True") => Ok(true),
        Some("0" | "f" | "F" | "FALSE" | "false" | "False") => Ok(false),
        Some(v) => Err(v.to_string()),
    })
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

/// Prints the compiled code of a program.
fn dis(path: &Path) -> ExitCode {
    let lang = haru_core::lang::for_path(path);
    let Ok(source) = std::fs::read_to_string(path) else {
        eprintln!("haru: cannot read {}", path.display());
        return ExitCode::FAILURE;
    };
    let (program, _) = haru_syntax::parse(&source, lang.syntax);
    let resolver = packages::Resolver::new(packages::std_runtime(), Vec::new(), &[]);
    match haru_core::compiler::compile(&program, lang, &resolver) {
        Ok(p) => {
            for proto in &p.protos {
                println!("== {} ({} registers)", proto.name, proto.nregs);
                for (i, op) in proto.code.iter().enumerate() {
                    println!("{i:4}  {op:?}");
                }
            }
            ExitCode::SUCCESS
        }
        Err(u) => {
            eprintln!("haru: not supported yet: {}", u.0);
            ExitCode::from(3)
        }
    }
}
