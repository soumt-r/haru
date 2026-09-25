//! `haru ast <file>` prints the syntax tree; `haru ast-check <dir>` compares
//! Haru's trees with Hana's (written by `tools/astdump`).

use std::path::Path;
use std::process::ExitCode;

use haru_syntax::{dump, parse, profile_for};

pub fn print(path: &Path) -> ExitCode {
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("haru: {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    };
    let (program, diags) = parse(&source, profile_for(path));
    println!("{}", dump::program(&program));
    for d in diags {
        eprintln!("{}:{}:{}: unexpected {:?}", path.display(), d.line, d.col + 1, d.literal);
    }
    ExitCode::SUCCESS
}

pub fn check(dir: &Path) -> ExitCode {
    let index = std::fs::read_to_string(dir.join("index.tsv")).unwrap_or_default();
    let (mut total, mut failed) = (0, 0);
    for line in index.lines() {
        let Some((file, origin)) = line.split_once('\t') else { continue };
        let path = dir.join(file);
        let source = std::fs::read_to_string(&path).unwrap_or_default();
        let want = std::fs::read_to_string(path.with_extension("json")).unwrap_or_default();
        let (program, diags) = parse(&source, profile_for(&path));
        let mut got = dump::program(&program);
        for d in diags {
            got.push_str(&format!("\n{}:{}:{}:{}", d.line, d.col, d.length, d.literal));
        }
        total += 1;
        if got != want {
            failed += 1;
            if failed <= 20 {
                let at = got.bytes().zip(want.bytes()).take_while(|(a, b)| a == b).count();
                let from = floor_char(&got, at.saturating_sub(60));
                eprintln!("MISMATCH {file} ({origin})");
                eprintln!("  haru: …{}", excerpt(&got, from));
                eprintln!("  hana: …{}", excerpt(&want, floor_char(&want, from)));
            }
        }
    }
    println!("{} / {} programs parse the same as Hana", total - failed, total);
    if failed == 0 && total > 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn floor_char(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn excerpt(s: &str, from: usize) -> &str {
    &s[from..floor_char(s, from + 200)]
}
