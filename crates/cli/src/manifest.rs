//! `haru.toml`: a package's manifest, and a project's (the same file: a
//! project is a package that other packages do not depend on).
//!
//! ```toml
//! [package]
//! name = "github.com/owner/repo"
//! version = "1.2.0"
//!
//! [dependencies]                  # the lowest version each needs (MVS)
//! "github.com/owner/other" = "0.3.0"
//!
//! [replace]                       # a project's own: a local folder instead
//! "github.com/owner/other" = "../other"
//!
//! [entry]                         # source entry points (these are the defaults)
//! hari = "hari/index.hr"
//! kanade = "kanade/index.knd"
//!
//! [native]                        # a module written with haru-sdk
//! id = "other"                    # its haru_module_v1_<id>
//! crate = "native"                # built from source by `haru install`, and/or
//! [native.files.windows-amd64]    # prebuilt for a platform
//! file = "native/other.dll"
//! url = "https://.../other.dll"   # downloaded by `haru install`
//! sha256 = "..."
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const FILE: &str = "haru.toml";
pub const LOCK_FILE: &str = "haru.lock";

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<PackageSection>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub replace: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<EntrySection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<NativeSection>,
    /// Packages whose native crate a project lets `haru install` build (a
    /// build runs the crate's code).
    #[serde(default, rename = "trusted-builds", skip_serializing_if = "Vec::is_empty")]
    pub trusted_builds: Vec<String>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageSection {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EntrySection {
    pub hari: Option<String>,
    pub kanade: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NativeSection {
    pub id: Option<String>,
    #[serde(rename = "crate")]
    pub krate: Option<String>,
    #[serde(default)]
    pub files: BTreeMap<String, NativeFile>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NativeFile {
    pub file: String,
    pub url: Option<String>,
    pub sha256: Option<String>,
}

impl Manifest {
    /// `dir/haru.toml`; `Ok(None)` when there is none.
    pub fn load(dir: &Path) -> Result<Option<Manifest>, String> {
        let text = match std::fs::read_to_string(dir.join(FILE)) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        Manifest::parse(&text).map(Some)
    }

    pub fn parse(text: &str) -> Result<Manifest, String> {
        let m: Manifest = toml::from_str(text).map_err(|e| e.message().to_string())?;
        for (path, v) in &m.dependencies {
            if !is_package_path(path) {
                return Err(format!("dependencies: {path:?} is not a package path like github.com/owner/repo"));
            }
            crate::semver::Version::parse(v).map_err(|e| format!("dependencies.{path}: {e}"))?;
        }
        for path in m.replace.keys() {
            if !is_package_path(path) {
                return Err(format!("replace: {path:?} is not a package path like github.com/owner/repo"));
            }
        }
        Ok(m)
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::write(dir.join(FILE), toml::to_string_pretty(self).expect("a manifest serializes"))
    }

    /// The entry point for a language, relative to the package folder.
    pub fn entry(&self, lang: &str) -> String {
        let declared = self.entry.as_ref().and_then(|e| if lang == "kanade" { e.kanade.clone() } else { e.hari.clone() });
        declared.unwrap_or_else(|| if lang == "kanade" { "kanade/index.knd".into() } else { "hari/index.hr".into() })
    }

    /// The native module's id (`haru_module_v1_<id>`).
    pub fn native_id(&self) -> Option<String> {
        let n = self.native.as_ref()?;
        Some(n.id.clone().unwrap_or_else(|| {
            let name = self.package.as_ref().map_or("", |p| p.name.as_str());
            name.rsplit('/').next().unwrap_or(name).replace('-', "_")
        }))
    }
}

/// A package's git path: a host with a dot, then an owner and a repository.
pub fn is_package_path(name: &str) -> bool {
    let mut parts = name.split('/');
    let Some(host) = parts.next() else { return false };
    let host_ok = host.contains('.')
        && host.split('.').all(|p| !p.is_empty() && p.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-'));
    let rest: Vec<&str> = parts.collect();
    host_ok
        && rest.len() >= 2
        && rest.iter().all(|p| {
            p.bytes().next().is_some_and(|c| c.is_ascii_alphanumeric())
                && p.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
        })
        && !name.ends_with(".git")
}

/// The platform a prebuilt native file is for (Go's names, as Hana uses).
pub fn platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        o => o,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        a => a,
    };
    format!("{os}-{arch}")
}

/// Where `haru install` puts the library it builds from a package's crate.
pub fn built_native(dir: &Path, id: &str) -> PathBuf {
    dir.join(".haru").join("native").join(platform()).join(haru_core::library_file_name(id))
}

/// One package as chosen: its exact version and the commit it came from.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LockEntry {
    pub version: String,
    pub commit: String,
}

pub type Lock = BTreeMap<String, LockEntry>;

pub fn load_lock(dir: &Path) -> Result<Lock, String> {
    match std::fs::read_to_string(dir.join(LOCK_FILE)) {
        Ok(t) => toml::from_str(&t).map_err(|e| format!("{LOCK_FILE}: {}", e.message())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Lock::new()),
        Err(e) => Err(e.to_string()),
    }
}

pub fn save_lock(dir: &Path, lock: &Lock) -> std::io::Result<()> {
    let text = format!("# Chosen by haru; commit it with haru.toml.\n{}", toml::to_string_pretty(lock).expect("a lock serializes"));
    std::fs::write(dir.join(LOCK_FILE), text)
}

/// The folder holding haru.toml at or above `start` (the project).
pub fn find_project(start: &Path) -> Option<PathBuf> {
    let mut dir = std::path::absolute(start).ok()?;
    loop {
        if dir.join(FILE).is_file() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Where downloaded packages live: `$HARU_HOME/pkg`, or `~/.haru/pkg`.
pub fn cache_root() -> Option<PathBuf> {
    if let Some(h) = std::env::var_os("HARU_HOME").filter(|h| !h.is_empty()) {
        return Some(PathBuf::from(h).join("pkg"));
    }
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })?;
    Some(PathBuf::from(home).join(".haru").join("pkg"))
}

/// One version of a package in the cache: `<cache>/<host>/<owner>/<repo>@<version>`.
pub fn cache_dir(path: &str, version: &str) -> Option<PathBuf> {
    let mut dir = cache_root()?;
    for part in path.split('/') {
        dir.push(part);
    }
    let last = format!("{}@{version}", dir.file_name()?.to_string_lossy());
    dir.set_file_name(last);
    Some(dir)
}
