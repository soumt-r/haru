//! `haru run [--time] <file>`: reports the way `hana run` does (syntax errors
//! on standard output, runtime errors on standard error, exit status 1), so
//! the two can be compared. A construct Haru cannot run yet exits with 3.
//! `--time` prints how long reading and running took, measured in-process.

use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use haru_core::vm::{codes, Vm};
use haru_core::{compiler, lang, syntax_report};

use crate::packages::{Bundled, Resolver};

pub fn run(path: &Path, time: bool, extra: Vec<Bundled>, files: &'static [(&'static str, &'static str)]) -> ExitCode {
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
            eprintln!("{}", e.report(Some(&rt), lang));
            ExitCode::FAILURE
        }
    };
    if time {
        eprintln!("\nparse+compile: {:?}\nrun: {:?}", compiled_at - start, done - compiled_at);
    }
    code
}
