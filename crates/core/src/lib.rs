//! Haru runtime core. See `DESIGN.md` at the repository root.
//!
//! Values in the ABI layout, the module registry and native calls (M0), and
//! the compiler and register machine that run programs (M2).

mod builtins;
pub mod bytecode;
mod catalog;
pub mod compiler;
mod dylib;
mod error;
pub mod gc;
pub mod format;
mod host;
pub mod lang;
mod modules;
pub mod symbol;
pub(crate) mod stack;
pub mod value;
pub mod vm;

pub use dylib::library_file_name;
pub use haru_abi::EntryFn;
pub use error::RuntimeError;
pub use modules::{FnRef, FunctionInfo, LoadError, ModuleInfo, Runtime};
pub use value::Value;

/// Why a program did not run to its end.
#[derive(Debug)]
pub enum RunError {
    /// Syntax errors, in the program's language.
    Syntax(Vec<haru_syntax::Diagnostic>),
    /// A construct this version cannot run yet.
    Unsupported(String),
    Runtime(RuntimeError),
}

/// What Hana's CLI prints for syntax errors: the heading and one message per
/// problem, in the program's language.
pub fn syntax_report(diags: &[haru_syntax::Diagnostic], lang: &lang::Lang) -> (&'static str, Vec<String>) {
    let label = match lang.locale {
        1 => catalog::PARSE_LABEL.1,
        2 => catalog::PARSE_LABEL.2,
        _ => catalog::PARSE_LABEL.0,
    };
    let messages = diags
        .iter()
        .map(|d| {
            let e = if d.literal.is_empty() {
                RuntimeError::core("SyntaxError.UnexpectedEnd").num_arg(d.line as f64)
            } else {
                RuntimeError::core("SyntaxError.UnexpectedToken")
                    .num_arg(d.line as f64)
                    .num_arg(d.col as f64 + 1.0)
                    .str_arg(&d.literal)
            };
            e.localize(None, lang)
        })
        .collect();
    (label, messages)
}

/// Runs a program with no input and returns what it printed, followed by the
/// CLI's error line when it failed (for tests).
pub fn run_to_string(source: &str, lang: &'static lang::Lang) -> String {
    run_to_string_with(source, lang, false)
}

/// `run_to_string`, with the JIT on (`jit`) or off.
pub fn run_to_string_with(source: &str, lang: &'static lang::Lang, jit: bool) -> String {
    let (program, diags) = haru_syntax::parse(source, lang.syntax);
    if !diags.is_empty() {
        let (label, messages) = syntax_report(&diags, lang);
        return format!("{label}:\n{}", messages.iter().map(|m| format!("  - {m}\n")).collect::<String>());
    }
    let compiled = match compiler::compile(&program, lang, &compiler::NoPackages) {
        Ok(c) => c,
        Err(u) => return format!("unsupported: {}", u.0),
    };
    let mut vm = vm::Vm::new(&compiled);
    if jit {
        vm.enable_jit_at(0);
    }
    vm.output = vm::Output::Capture(String::new());
    let result = vm.run();
    let vm::Output::Capture(mut out) = std::mem::replace(&mut vm.output, vm::Output::Capture(String::new())) else {
        unreachable!()
    };
    if let Err(e) = result {
        out.push_str(&e.report(None, lang));
        out.push('\n');
    }
    out
}

/// Runs a compiled program with native modules and returns what it printed,
/// followed by the CLI's error line when it failed (for tests).
pub fn run_compiled_to_string(compiled: &bytecode::Program, lang: &'static lang::Lang, rt: &Runtime) -> String {
    let mut vm = vm::Vm::new(compiled).with_runtime(rt);
    vm.output = vm::Output::Capture(String::new());
    let result = vm.run();
    let vm::Output::Capture(mut out) = std::mem::replace(&mut vm.output, vm::Output::Capture(String::new())) else {
        unreachable!()
    };
    if let Err(e) = result {
        out.push_str(&e.report(Some(rt), lang));
        out.push('\n');
    }
    out
}

/// Parses, compiles and runs a program, printing to standard output.
pub fn run_source(source: &str, lang: &'static lang::Lang) -> Result<(), RunError> {
    let (program, diags) = haru_syntax::parse(source, lang.syntax);
    if !diags.is_empty() {
        return Err(RunError::Syntax(diags));
    }
    let compiled = compiler::compile(&program, lang, &compiler::NoPackages).map_err(|u| RunError::Unsupported(u.0))?;
    let mut vm = vm::Vm::new(&compiled);
    let stdin = std::io::stdin();
    vm.read_line = Box::new(move || {
        let mut line = String::new();
        match stdin.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line.trim_end_matches(['\r', '\n']).to_string()),
        }
    });
    vm.run().map_err(RunError::Runtime)
}
