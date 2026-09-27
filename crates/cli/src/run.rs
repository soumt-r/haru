//! `haru run [--time] <file>`: reports the way `hana run` does (syntax errors
//! on standard output, runtime errors on standard error, exit status 1), so
//! the two can be compared. A construct Haru cannot run yet exits with 3.
//! `--time` prints how long reading and running took, measured in-process.
//! Functions that run often become native code (the JIT) unless `--no-jit`
//! or `HARU_JIT=0` says not to (`HARU_JIT=1` and `--jit` ask for it, as
//! before it was the default).

use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use haru_core::vm::{codes, Vm};
use haru_core::{compiler, lang, syntax_report};

use crate::packages::{Bundled, Resolver};

/// Whether the JIT is wanted: yes, unless `--no-jit` was given.
pub static JIT_FLAG: AtomicBool = AtomicBool::new(true);

fn jit_wanted() -> bool {
    match std::env::var("HARU_JIT").as_deref() {
        Ok("0") => false,
        Ok("1") => true,
        _ => JIT_FLAG.load(Ordering::Relaxed),
    }
}

pub fn run(path: &Path, time: bool, extra: Vec<Bundled>, files: &'static [(&'static str, &'static str)]) -> ExitCode {
    if !jit_wanted() {
        return run_here(path, time, extra, files, false);
    }
    // Compiled code calls functions on the machine's stack: a deep one
    // (reserved, not committed) so that Hana's call depth limit comes first.
    let path = path.to_path_buf();
    std::thread::Builder::new()
        .stack_size(1 << 30)
        .spawn(move || run_here(&path, time, extra, files, true))
        .ok()
        .and_then(|t| t.join().ok())
        .unwrap_or(ExitCode::FAILURE)
}

fn run_here(path: &Path, time: bool, extra: Vec<Bundled>, files: &'static [(&'static str, &'static str)], jit: bool) -> ExitCode {
    let lang = lang::for_path(path);
    let embedded = files.iter().find(|(p, _)| Path::new(p) == path).map(|(_, t)| t.to_string());
    let source = match embedded.map_or_else(|| std::fs::read_to_string(path), Ok) {
        Ok(s) => s,
        Err(e) => {
            println!("{}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    };

    let start = Instant::now();
    let (program, diags) = haru_syntax::parse(&source, lang.syntax);
    if !diags.is_empty() {
        let (label, messages) = syntax_report(&diags, lang);
        println!("{label}:");
        for m in messages {
            println!("  - {m}");
        }
        return ExitCode::FAILURE;
    }
    let resolver = Resolver::new(crate::packages::std_runtime(), extra, files);
    let compiled = match compiler::compile(&program, lang, &resolver) {
        Ok(c) => c,
        Err(u) => {
            eprintln!("haru: not supported yet: {}", u.0);
            return ExitCode::from(3);
        }
    };
    let compiled_at = Instant::now();

    let rt = resolver.rt.borrow();
    let mut vm = Vm::new(&compiled).with_runtime(&rt);
    if jit {
        vm.enable_jit();
    }
    let stdin = std::io::stdin();
    vm.read_line = Box::new(move || {
        let mut line = String::new();
        match stdin.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line.trim_end_matches(['\r', '\n']).to_string()),
        }
    });
    let result = vm.run();
    // Up to the end of the program, not the final write of its output (as
    // Hana times it).
    let done = vm.finished.unwrap_or_else(Instant::now);

    let code = match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.code == codes::UNSUPPORTED => {
            eprintln!("haru: not supported yet: {}", e.args.first().map(|a| a.to_string()).unwrap_or_default());
            return ExitCode::from(3);
        }
        Err(e) => {
            eprintln!("{}", e.report(Some(&rt), lang));
            ExitCode::FAILURE
        }
    };
    if std::env::var_os("HARU_GC_STATS").is_some() {
        let (runs, freed) = haru_core::gc::stats();
        eprintln!("gc: {runs} collection(s), {freed} container(s) freed");
    }
    if time {
        eprintln!("\nparse+compile: {:?}\nrun: {:?}", compiled_at - start, done - compiled_at);
    }
    code
}
