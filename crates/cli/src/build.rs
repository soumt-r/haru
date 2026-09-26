//! `haru build [--with <package>]... [-o <file>] [<program>]`: one executable.
//!
//! The packages' native crates are compiled into it with Haru (no dynamic
//! library to ship or load, link-time optimization across them), and their
//! source entry points are carried inside. With a program, the binary runs
//! that program — carried inside too, with the files it imports — instead of
//! being the `haru` command.
//!
//! A package is a folder with a haru.toml, or a name `haru run` would find
//! (an installed `github.com/owner/repo`, a `packages/<이름>` folder). Its
//! crate must build as a Rust library (`crate-type = ["cdylib", "rlib"]`).
//! Building needs Haru's source: where this `haru` was built from, or `$HARU_SRC`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use crate::manifest::{self, Manifest};

type Res<T> = Result<T, String>;

pub fn command(args: &[String]) -> ExitCode {
    match build(args) {
        Ok(out) => {
            println!("{}", out.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("haru: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Where Haru's source is.
fn haru_source() -> Res<PathBuf> {
    let dir = match std::env::var_os("HARU_SRC") {
        Some(d) => PathBuf::from(d),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(".."),
    };
    let dir = std::path::absolute(&dir).map_err(|e| e.to_string())?;
    if !dir.join("crates").join("cli").join("Cargo.toml").is_file() {
        return Err(format!("haru build needs Haru's source; {} does not hold it (set HARU_SRC)", dir.display()));
    }
    Ok(dir)
}

/// A package to link: its import name, folder and manifest.
struct Linked {
    name: String,
    dir: PathBuf,
    manifest: Manifest,
}

fn find_package(arg: &str) -> Res<Linked> {
    let path = Path::new(arg);
    let (name, dir) = if path.join(manifest::FILE).is_file() {
        let m = Manifest::load(path)?.unwrap_or_default();
        let name = m.package.as_ref().map(|p| p.name.clone()).filter(|n| !n.is_empty());
        (name.ok_or_else(|| format!("{arg}: haru.toml names no package ([package] name)"))?, path.to_path_buf())
    } else {
        let dir = crate::packages::locate_folder(arg)?.ok_or_else(|| format!("no package {arg} (a folder with haru.toml, or a package haru run finds)"))?;
        (arg.to_string(), dir)
    };
    let dir = std::path::absolute(&dir).map_err(|e| e.to_string())?;
    let manifest = Manifest::load(&dir)?.unwrap_or_default();
    Ok(Linked { name, dir, manifest })
}

/// A Rust string literal of any text (a path).
fn lit(s: &str) -> String {
    format!("{s:?}")
}

fn include(path: &Path) -> String {
    format!("include_str!({})", lit(&path.display().to_string()))
}

/// The crate of a package's native module: (Cargo package name, library name).
fn crate_names(crate_dir: &Path) -> Res<(String, String)> {
    let text = std::fs::read_to_string(crate_dir.join("Cargo.toml")).map_err(|e| format!("{}: {e}", crate_dir.display()))?;
    let t: toml::Table = toml::from_str(&text).map_err(|e| format!("{}: Cargo.toml: {}", crate_dir.display(), e.message()))?;
    let package = t.get("package").and_then(|p| p.get("name")).and_then(|n| n.as_str()).ok_or("its Cargo.toml has no package name")?;
    let lib = t.get("lib");
    let lib_name = lib.and_then(|l| l.get("name")).and_then(|n| n.as_str()).map_or_else(|| package.replace('-', "_"), str::to_string);
    if let Some(types) = lib.and_then(|l| l.get("crate-type")).and_then(|c| c.as_array()) {
        if !types.iter().any(|c| matches!(c.as_str(), Some("rlib" | "lib"))) {
            return Err(format!("{}: add \"rlib\" to [lib] crate-type so it can be linked in", crate_dir.display()));
        }
    }
    Ok((package.to_string(), lib_name))
}

fn build(args: &[String]) -> Res<PathBuf> {
    let mut with = Vec::new();
    let mut out: Option<PathBuf> = None;
    let mut program: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--with" => {
                i += 1;
                with.push(args.get(i).ok_or("--with needs a package")?.clone());
            }
            "-o" => {
                i += 1;
                out = Some(PathBuf::from(args.get(i).ok_or("-o needs a file name")?));
            }
            a if program.is_none() && !a.starts_with('-') => program = Some(a.to_string()),
            a => return Err(format!("unexpected {a}; usage: haru build [--with <package>]... [-o <file>] [<program>]")),
        }
        i += 1;
    }
    let src = haru_source()?;
    let linked = with.iter().map(|w| find_package(w)).collect::<Res<Vec<_>>>()?;

    // The generated crate.
    let bin = out
        .as_ref()
        .and_then(|o| o.file_stem())
        .or_else(|| program.as_ref().and_then(|p| Path::new(p).file_stem()))
        .map_or("haru-with".to_string(), |s| s.to_string_lossy().to_string());
    let bin: String = bin.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    let bin = if bin.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) { bin } else { format!("haru-{bin}") };
    let work = src.join("target").join("haru-build").join(&bin);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;

    let mut deps = format!("haru-cli = {{ path = {} }}\n", lit(&src.join("crates").join("cli").display().to_string()));
    let mut packages = String::new();
    for (n, p) in linked.iter().enumerate() {
        let native = match (&p.manifest.native, p.manifest.native.as_ref().and_then(|n| n.krate.as_ref())) {
            (Some(_), Some(k)) => {
                let crate_dir = p.dir.join(k);
                let (package, _) = crate_names(&crate_dir)?;
                writeln!(deps, "p{n} = {{ path = {}, package = {} }}", lit(&crate_dir.display().to_string()), lit(&package)).unwrap();
                format!("Some(p{n}::haru_entry)")
            }
            (Some(_), None) => return Err(format!("{}: its native module has no crate to link (only prebuilt files)", p.name)),
            (None, _) => "None".to_string(),
        };
        let entry = |lang: &str| {
            let f = p.dir.join(p.manifest.entry(lang));
            if f.is_file() { format!("Some({})", include(&f)) } else { "None".to_string() }
        };
        writeln!(
            packages,
            "        haru_cli::Bundled {{ name: {}, hari: {}, kanade: {}, native: {native}, unsupported: false }},",
            lit(&p.name),
            entry("hari"),
            entry("kanade")
        )
        .unwrap();
    }

    let program_code = match &program {
        None => "None".to_string(),
        Some(main) => {
            let files = program_files(main, &with)?;
            let mut list = String::new();
            for f in &files {
                let abs = std::path::absolute(f).map_err(|e| e.to_string())?;
                writeln!(list, "            ({}, {}),", lit(f), include(&abs)).unwrap();
            }
            format!("Some(haru_cli::Embedded {{\n        main: {},\n        files: &[\n{list}        ],\n    }})", lit(main))
        }
    };

    let cargo_toml = format!(
        "[package]\nname = \"haru-build-{bin}\"\nversion = \"0.0.0\"\nedition = \"2021\"\npublish = false\n\n[[bin]]\nname = {}\npath = \"main.rs\"\n\n[dependencies]\n{deps}\n[workspace]\n\n[profile.release]\nlto = \"fat\"\ncodegen-units = 1\npanic = \"abort\"\n",
        lit(&bin)
    );
    let main_rs = format!(
        "// Generated by `haru build`.\nfn main() -> std::process::ExitCode {{\n    haru_cli::main_with(haru_cli::Build {{\n        packages: vec![\n{packages}        ],\n        program: {program_code},\n    }})\n}}\n"
    );
    std::fs::write(work.join("Cargo.toml"), cargo_toml).map_err(|e| e.to_string())?;
    std::fs::write(work.join("main.rs"), main_rs).map_err(|e| e.to_string())?;

    eprintln!("haru: building {bin} (the first build compiles Haru; later ones are quicker)");
    let target = src.join("target").join("haru-build").join("target");
    let status = Command::new("cargo")
        .args(["build", "--release", "--quiet"])
        .current_dir(&work)
        .env("CARGO_TARGET_DIR", &target)
        .status()
        .map_err(|e| format!("cargo could not run ({e}); is Rust installed?"))?;
    if !status.success() {
        return Err("the build failed".into());
    }
    let built = target.join("release").join(format!("{bin}{}", std::env::consts::EXE_SUFFIX));
    let out = out.unwrap_or_else(|| PathBuf::from(format!("{bin}{}", std::env::consts::EXE_SUFFIX)));
    std::fs::copy(&built, &out).map_err(|e| format!("{}: {e}", out.display()))?;
    Ok(out)
}

/// The program's main file and every file it imports, as their imports name them.
fn program_files(main: &str, with: &[String]) -> Res<Vec<String>> {
    let path = Path::new(main);
    let lang = haru_core::lang::for_path(path);
    let source = std::fs::read_to_string(path).map_err(|e| format!("{main}: {e}"))?;
    let (program, diags) = haru_syntax::parse(&source, lang.syntax);
    if !diags.is_empty() {
        return Err(format!("{main} has syntax errors (haru run {main} shows them)"));
    }
    let _ = with;
    let resolver = crate::packages::Resolver::new(crate::packages::std_runtime(), Vec::new(), &[]);
    let compiled = haru_core::compiler::compile(&program, lang, &resolver).map_err(|u| format!("not supported yet: {}", u.0))?;
    let mut files = vec![main.to_string()];
    for m in &compiled.modules {
        if let Some(f) = m.key.strip_prefix("file:") {
            if Path::new(f).is_file() && !files.iter().any(|x| x == f) {
                files.push(f.to_string());
            }
        }
    }
    Ok(files)
}
