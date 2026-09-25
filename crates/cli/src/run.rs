//! `haru run [--time] <file>`: reports the way `hana run` does (syntax errors
//! on standard output, runtime errors on standard error, exit status 1), so
//! the two can be compared. A construct Haru cannot run yet exits with 3.
//! `--time` prints how long reading and running took, measured in-process.

use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use haru_core::vm::{codes, Vm};
use haru_core::{compiler, lang, syntax_report};

pub fn run(path: &Path, time: bool) -> ExitCode {
    let lang = lang::for_path(path);
    let source = match std::fs::read_to_string(path) {
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
    let compiled = match compiler::compile(&program, lang) {
        Ok(c) => c,
        Err(u) => {
            eprintln!("haru: not supported yet: {}", u.0);
            return ExitCode::from(3);
        }
    };
    let compiled_at = Instant::now();

    let mut vm = Vm::new(&compiled);
    let stdin = std::io::stdin();
    vm.read_line = Box::new(move || {
        let mut line = String::new();
        match stdin.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line.trim_end_matches(['\r', '\n']).to_string()),
        }
    });
    let result = vm.run();
    let done = Instant::now();

    let code = match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.code == codes::UNSUPPORTED => {
            eprintln!("haru: not supported yet: {}", e.args.first().map(|a| a.to_string()).unwrap_or_default());
            return ExitCode::from(3);
        }
        Err(e) => {
            eprintln!("{}", e.report(None, lang));
            ExitCode::FAILURE
        }
    };
    if time {
        eprintln!("\nparse+compile: {:?}\nrun: {:?}", compiled_at - start, done - compiled_at);
    }
    code
}
