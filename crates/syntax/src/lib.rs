//! Hari and Kanade syntax: lexer, parser and tree. The grammar is Hana's —
//! Haru and Hana must read every program the same way.

pub mod ast;
pub mod dump;
pub mod lexer;
pub mod parser;
pub mod profile;
pub mod token;

pub use parser::{DiagKind, Diagnostic, Parser};
pub use profile::{Profile, HARI, KANADE};

/// The profile for a source file, by extension (`.knd` is Kanade).
pub fn profile_for(path: &std::path::Path) -> &'static Profile {
    match path.extension().and_then(|e| e.to_str()) {
        Some("knd") => &KANADE,
        _ => &HARI,
    }
}

/// Parses a whole program.
pub fn parse(source: &str, profile: &'static Profile) -> (ast::Program, Vec<Diagnostic>) {
    let mut p = Parser::new(lexer::tokenize(source, profile), profile);
    let program = p.parse_program();
    (program, p.diagnostics())
}
