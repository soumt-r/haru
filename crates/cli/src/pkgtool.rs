//! `haru init / add / install / remove / list`: a project's packages.
//!
//! Versions are git tags (`v1.2.0` or `1.2.0`) of the package's repository.
//! Each package names the lowest version of each package it needs, and every
//! package gets the highest version anyone needs (minimal version selection,
//! as Hana and Go do: versions are minimums, so there is never a conflict).
//! The choice goes into `haru.lock`; `haru run` only reads it.
//!
//! A downloaded package lives in `~/.haru/pkg/<host>/<owner>/<repo>@<version>`
//! (`$HARU_HOME/pkg`). Its native module is prepared there too: a prebuilt
//! file for this platform is downloaded and checked against its sha256, and a
//! crate is built with cargo — only for packages the project trusts
//! (`trusted-builds`, or `haru add --allow-build`), because a build runs the
//! crate's code.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use sha2::{Digest, Sha256};

use crate::manifest::{self, is_package_path, Lock, LockEntry, Manifest};
use crate::semver::Version;

/// The file in a downloaded package folder naming the commit it came from.
const COMMIT_FILE: &str = ".haru-commit";

type Res<T> = Result<T, String>;

/// A git tag that reads as a version.
#[derive(Clone)]
struct Tag {
    name: String,
    version: Version,
}

fn repo_url(path: &str) -> String {
    if std::env::var("HARU_GIT_PROTOCOL").is_ok_and(|p| p == "ssh") {
        if let Some((host, rest)) = path.split_once('/') {
            return format!("git@{host}:{rest}.git");
        }
    }
    format!("https://{path}.git")
}

fn git(dir: Option<&Path>, args: &[&str]) -> Res<String> {
    let mut cmd = Command::new("git");
    cmd.args(args).env("GIT_TERMINAL_PROMPT", "0");
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    let out = cmd.output().map_err(|e| format!("git could not run ({e}); is git installed?"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("git {}: {}", args.first().unwrap_or(&""), err.lines().next().unwrap_or("").trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The versions of a package, highest first.
fn tags(path: &str) -> Res<Vec<Tag>> {
    let out = git(None, &["ls-remote", "--tags", &repo_url(path)])?;
    let mut by_name: BTreeMap<String, (Option<String>, Option<String>)> = BTreeMap::new();
    for line in out.lines() {
        let Some((commit, reference)) = line.split_once('\t') else { continue };
        let Some(name) = reference.strip_prefix("refs/tags/") else { continue };
        // An annotated tag is listed twice; the ^{} line is the commit.
        match name.strip_suffix("^{}") {
            Some(n) => by_name.entry(n.to_string()).or_default().1 = Some(commit.to_string()),
            None => by_name.entry(name.to_string()).or_default().0 = Some(commit.to_string()),
        }
    }
    let mut tags: Vec<Tag> = by_name
        .into_iter()
        .filter_map(|(name, (direct, peeled))| {
            let version = Version::parse(&name).ok()?;
            peeled.or(direct)?;
            Some(Tag { name, version })
        })
        .collect();
    tags.sort_by(|a, b| b.version.cmp(&a.version));
    Ok(tags)
}

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

struct Installer {
    project: PathBuf,
    manifest: Manifest,
    /// Packages whose crate may be built now (`--allow-build`).
    allow_build: BTreeSet<String>,
    tag_cache: BTreeMap<String, Vec<Tag>>,
}

impl Installer {
    fn tags(&mut self, path: &str) -> Res<Vec<Tag>> {
        if let Some(t) = self.tag_cache.get(path) {
            return Ok(t.clone());
        }
        let t = tags(path)?;
        self.tag_cache.insert(path.to_string(), t.clone());
        Ok(t)
    }

    fn latest(&mut self, path: &str) -> Res<Version> {
        self.tags(path)?.first().map(|t| t.version).ok_or_else(|| format!("{path} has no version tags (like v1.0.0)"))
    }

    fn replace_dir(&self, path: &str) -> Option<PathBuf> {
        self.manifest.replace.get(path).map(|d| self.project.join(d))
    }

    fn trusts(&self, path: &str) -> bool {
        self.allow_build.contains(path) || self.manifest.trusted_builds.iter().any(|p| p == path)
    }

    /// The folder of path@version in the cache, downloading it when needed; a
    /// commit from the lock file must match.
    fn ensure(&mut self, path: &str, v: Version, want_commit: Option<&str>) -> Res<(PathBuf, String)> {
        let dir = manifest::cache_dir(path, &v.to_string()).ok_or("no home folder for the package cache")?;
        if let Ok(have) = std::fs::read_to_string(dir.join(COMMIT_FILE)) {
            let have = have.trim().to_string();
            if let Some(want) = want_commit.filter(|w| *w != have) {
                return Err(format!("{path}@{v}: haru.lock says commit {want:.12}, the cache holds {have:.12}"));
            }
            self.prepare(path, &dir)?;
            return Ok((dir, have));
        }
        let tag = self.tags(path)?.into_iter().find(|t| t.version == v).ok_or_else(|| format!("{path} has no version {v}"))?;
        eprintln!("haru: downloading {path}@{v}");
        let parent = dir.parent().ok_or("bad cache folder")?;
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let work = parent.join(format!(".download-{}-{}", dir.file_name().unwrap().to_string_lossy(), std::process::id()));
        let _ = std::fs::remove_dir_all(&work);
        let work_s = work.to_string_lossy().to_string();
        git(None, &["clone", "--quiet", "--depth", "1", "--branch", &tag.name, &repo_url(path), &work_s])?;
        let commit = git(Some(&work), &["rev-parse", "HEAD"])?.trim().to_string();
        if let Some(want) = want_commit.filter(|w| *w != commit) {
            let _ = std::fs::remove_dir_all(&work);
            return Err(format!("{path}@{v}: the tag now points at {commit:.12}, haru.lock says {want:.12}"));
        }
        std::fs::remove_dir_all(work.join(".git")).map_err(|e| e.to_string())?;
        std::fs::write(work.join(COMMIT_FILE), format!("{commit}\n")).map_err(|e| e.to_string())?;
        std::fs::rename(&work, &dir).map_err(|e| e.to_string())?;
        self.prepare(path, &dir)?;
        Ok((dir, commit))
    }

    /// A package's manifest, checked to be the package it should be.
    fn manifest_of(path: &str, dir: &Path) -> Res<Manifest> {
        let m = Manifest::load(dir).map_err(|e| format!("{path}: haru.toml: {e}"))?.unwrap_or_default();
        if let Some(name) = m.package.as_ref().map(|p| p.name.as_str()).filter(|n| !n.is_empty()) {
            if name != path {
                return Err(format!("{path}: its haru.toml says it is {name}"));
            }
        }
        Ok(m)
    }

    /// Makes a package's native module usable here.
    fn prepare(&self, path: &str, dir: &Path) -> Res<()> {
        let m = Self::manifest_of(path, dir)?;
        prepare_native(path, dir, &m, self.trusts(path))
    }

    /// Chooses a version for everything the roots need (MVS), downloading
    /// what it looks at. Replaced packages are followed but not locked.
    fn resolve(&mut self, roots: &BTreeMap<String, String>) -> Res<Lock> {
        let mut best: BTreeMap<String, (Version, String)> = BTreeMap::new();
        let mut visited = BTreeSet::new();
        let mut stack = Vec::new();
        for (path, v) in roots {
            let v = Version::parse(v)?;
            self.visit(path, v, &mut stack, &mut visited, &mut best)?;
        }
        Ok(best.into_iter().map(|(p, (v, commit))| (p, LockEntry { version: v.to_string(), commit })).collect())
    }

    fn visit(
        &mut self,
        path: &str,
        v: Version,
        stack: &mut Vec<String>,
        visited: &mut BTreeSet<(String, String)>,
        best: &mut BTreeMap<String, (Version, String)>,
    ) -> Res<()> {
        if stack.iter().any(|p| p == path) {
            return Err(format!("packages need each other: {} -> {path}", stack.join(" -> ")));
        }
        let replaced = self.replace_dir(path);
        let key = (path.to_string(), if replaced.is_some() { String::new() } else { v.to_string() });
        if !visited.insert(key) {
            return Ok(());
        }
        let dir = match replaced {
            Some(d) => {
                // The project's own folder: its native module is prepared as it is.
                let m = Self::manifest_of(path, &d)?;
                prepare_native(path, &d, &m, true)?;
                d
            }
            None => {
                let (dir, commit) = self.ensure(path, v, None)?;
                if best.get(path).is_none_or(|(b, _)| v > *b) {
                    best.insert(path.to_string(), (v, commit));
                }
                dir
            }
        };
        let m = Self::manifest_of(path, &dir)?;
        stack.push(path.to_string());
        for (dep, dv) in &m.dependencies {
            let dv = Version::parse(dv).map_err(|e| format!("{path}: {e}"))?;
            self.visit(dep, dv, stack, visited, best)?;
        }
        stack.pop();
        Ok(())
    }

    /// Whether the lock covers every dependency at its minimum.
    fn covers(&self, lock: &Lock) -> bool {
        self.manifest.dependencies.iter().all(|(path, min)| {
            self.manifest.replace.contains_key(path)
                || lock.get(path).is_some_and(|e| {
                    matches!((Version::parse(&e.version), Version::parse(min)), (Ok(have), Ok(want)) if have >= want)
                })
        })
    }

    fn save(&self, lock: &Lock) -> Res<()> {
        self.manifest.save(&self.project).map_err(|e| e.to_string())?;
        manifest::save_lock(&self.project, lock).map_err(|e| e.to_string())
    }
}

/// Makes a package's native module usable: a prebuilt file for this platform
/// (downloaded and checked when it is not there), or its crate built.
fn prepare_native(path: &str, dir: &Path, m: &Manifest, trusted: bool) -> Res<()> {
    let (Some(native), Some(id)) = (m.native.as_ref(), m.native_id()) else { return Ok(()) };
    let platform = manifest::platform();
    if let Some(f) = native.files.get(&platform) {
        let file = dir.join(&f.file);
        if file.is_file() {
            return Ok(());
        }
        if let Some(url) = &f.url {
            let want = f.sha256.as_ref().ok_or_else(|| format!("{path}: the {platform} library has a url but no sha256"))?;
            eprintln!("haru: downloading the {platform} library of {path}");
            let data = haru_std::fetch(url).map_err(|e| format!("{path}: {url}: {e}"))?;
            if !sha256_hex(&data).eq_ignore_ascii_case(want) {
                return Err(format!("{path}: the {platform} library does not match its sha256"));
            }
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            return std::fs::write(&file, data).map_err(|e| e.to_string());
        }
    }
    let Some(krate) = &native.krate else { return Ok(()) };
    let out = manifest::built_native(dir, &id);
    if out.is_file() {
        return Ok(());
    }
    if !trusted {
        eprintln!("haru: {path} has a native crate; building it runs its code. Trust it with `haru add {path} --allow-build`.");
        return Ok(());
    }
    build_crate(path, &dir.join(krate), &id, &out)
}

/// `cargo build --release` of a package's crate, its library copied to `out`.
fn build_crate(path: &str, crate_dir: &Path, id: &str, out: &Path) -> Res<()> {
    eprintln!("haru: building the native crate of {path}");
    let target = out.parent().unwrap().join("target");
    let status = Command::new("cargo")
        .args(["build", "--release", "--lib", "--quiet"])
        .current_dir(crate_dir)
        .env("CARGO_TARGET_DIR", &target)
        .status()
        .map_err(|e| format!("cargo could not run ({e}); is Rust installed?"))?;
    if !status.success() {
        return Err(format!("{path}: its native crate did not build"));
    }
    let built = target.join("release").join(haru_core::library_file_name(id));
    if !built.is_file() {
        return Err(format!("{path}: cargo built no {} (is the crate's lib named {id} with crate-type cdylib?)", built.display()));
    }
    std::fs::copy(&built, out).map_err(|e| e.to_string())?;
    Ok(())
}

fn installer(allow_build: BTreeSet<String>) -> Res<Installer> {
    let project = manifest::find_project(Path::new(".")).ok_or("no haru.toml here or above; `haru init` makes one")?;
    let manifest = Manifest::load(&project)?.unwrap_or_default();
    Ok(Installer { project, manifest, allow_build, tag_cache: BTreeMap::new() })
}

/// Runs a package command; `args` follow the command's name.
pub fn command(name: &str, args: &[String]) -> ExitCode {
    let result = match name {
        "init" => init(),
        "add" => add(args),
        "install" => install(),
        "remove" => remove(args),
        "list" => list(),
        _ => Err(format!("unknown command {name}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("haru: {e}");
            ExitCode::FAILURE
        }
    }
}

fn init() -> Res<()> {
    if Path::new(manifest::FILE).exists() {
        return Err("haru.toml is here already".into());
    }
    let name = std::env::current_dir().ok().and_then(|d| d.file_name().map(|n| n.to_string_lossy().to_string())).unwrap_or_default();
    let m = Manifest {
        package: Some(manifest::PackageSection { name, version: "0.1.0".into() }),
        ..Manifest::default()
    };
    m.save(Path::new(".")).map_err(|e| e.to_string())?;
    println!("haru.toml");
    Ok(())
}

fn add(args: &[String]) -> Res<()> {
    let allow = args.iter().any(|a| a == "--allow-build");
    let targets: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    if targets.is_empty() {
        return Err("usage: haru add <github.com/owner/repo>[@version] [--allow-build]".into());
    }
    let paths: BTreeSet<String> = targets.iter().map(|t| t.split('@').next().unwrap().to_string()).collect();
    let mut inst = installer(if allow { paths } else { BTreeSet::new() })?;
    for t in targets {
        let (path, version) = match t.split_once('@') {
            Some((p, v)) => (p.to_string(), Some(Version::parse(v)?)),
            None => (t.clone(), None),
        };
        if !is_package_path(&path) {
            return Err(format!("{path:?} is not a package path like github.com/owner/repo"));
        }
        let v = match version {
            Some(v) => v,
            None => inst.latest(&path)?,
        };
        inst.manifest.dependencies.insert(path.clone(), v.to_string());
        if allow && !inst.manifest.trusted_builds.contains(&path) {
            inst.manifest.trusted_builds.push(path.clone());
            inst.manifest.trusted_builds.sort();
        }
        println!("{path} {v}");
    }
    let deps = inst.manifest.dependencies.clone();
    let lock = inst.resolve(&deps)?;
    inst.save(&lock)
}

fn install() -> Res<()> {
    let mut inst = installer(BTreeSet::new())?;
    let mut lock = manifest::load_lock(&inst.project)?;
    if !inst.covers(&lock) {
        let deps = inst.manifest.dependencies.clone();
        lock = inst.resolve(&deps)?;
        manifest::save_lock(&inst.project, &lock).map_err(|e| e.to_string())?;
    }
    for (path, e) in &lock {
        let v = Version::parse(&e.version)?;
        inst.ensure(path, v, Some(&e.commit))?;
    }
    for (path, dir) in inst.manifest.replace.clone() {
        let d = inst.project.join(dir);
        let m = Installer::manifest_of(&path, &d)?;
        prepare_native(&path, &d, &m, true)?;
    }
    // The project's own local packages (./packages/<이름>) are the project's code.
    if let Ok(entries) = std::fs::read_dir(inst.project.join("packages")) {
        for e in entries.flatten().filter(|e| e.path().is_dir()) {
            let name = e.file_name().to_string_lossy().to_string();
            if let Ok(Some(m)) = Manifest::load(&e.path()) {
                prepare_native(&name, &e.path(), &m, true)?;
            }
        }
    }
    println!("{} package(s)", lock.len());
    Ok(())
}

fn remove(args: &[String]) -> Res<()> {
    let mut inst = installer(BTreeSet::new())?;
    let mut any = false;
    for path in args {
        any |= inst.manifest.dependencies.remove(path).is_some();
        inst.manifest.replace.remove(path);
        inst.manifest.trusted_builds.retain(|p| p != path);
    }
    if !any {
        return Err("none of those are dependencies of this project".into());
    }
    let deps = inst.manifest.dependencies.clone();
    let lock = inst.resolve(&deps)?;
    inst.save(&lock)
}

fn list() -> Res<()> {
    let inst = installer(BTreeSet::new())?;
    let lock = manifest::load_lock(&inst.project)?;
    for (path, e) in &lock {
        let direct = if inst.manifest.dependencies.contains_key(path) { "" } else { " (needed by others)" };
        println!("{path} {}{direct}", e.version);
    }
    for (path, dir) in &inst.manifest.replace {
        println!("{path} => {dir}");
    }
    Ok(())
}
