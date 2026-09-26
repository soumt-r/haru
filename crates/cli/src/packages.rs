//! What `[이름]` means to `haru run`: the resolver behind the compiler's
//! `Packages`. A name is looked for, in order:
//!
//! 1. a package: a git path (`github.com/owner/repo`) is the project's
//!    `[replace]` folder or the version `haru.lock` chose, in the cache; any
//!    other name is the first `packages/<이름>` folder of `./packages`, then
//!    of each folder in `$HARU_PACKAGES`, then of `packages/` next to `haru`;
//! 2. a package that comes with Haru (timezone);
//! 3. a standard module.
//!
//! A package folder holds its source entry points (`hari/index.hr`,
//! `kanade/index.knd`, or what its `haru.toml` says) and/or a native module.
//! Nothing is downloaded or built here: that is `haru install`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use haru_core::compiler::{Entry, Package, Packages};
use haru_core::Runtime;

use crate::manifest::{self, Manifest};

/// A runtime with the standard library.
pub fn std_runtime() -> Runtime {
    let mut rt = Runtime::new();
    for entry in haru_std::MODULES {
        rt.load_static(*entry).expect("the standard library loads");
    }
    rt
}

/// A package compiled into this binary: its entry points' text and native module.
pub struct Bundled {
    pub name: &'static str,
    pub hari: Option<&'static str>,
    pub kanade: Option<&'static str>,
    pub native: Option<haru_core::EntryFn>,
    /// Not ported to Haru yet (the program cannot run).
    pub unsupported: bool,
}

/// The packages that come with Haru.
pub fn bundled() -> Vec<Bundled> {
    vec![
        Bundled {
            name: "timezone",
            hari: Some(include_str!("../../../packages/timezone/hari/index.hr")),
            kanade: Some(include_str!("../../../packages/timezone/kanade/index.knd")),
            native: Some(timezone::haru_entry),
            unsupported: false,
        },
        Bundled { name: "http_server", hari: None, kanade: None, native: None, unsupported: true },
    ]
}

pub struct Resolver {
    pub rt: RefCell<Runtime>,
    bundled: Vec<Bundled>,
    /// What each name turned out to be (packages are looked at once).
    found: RefCell<HashMap<(String, String), Option<Package>>>,
    /// Native libraries loaded, by path (a module is registered once).
    loaded: RefCell<HashMap<PathBuf, Result<usize, (String, Vec<String>)>>>,
    project: Option<PathBuf>,
    /// Packages a build linked in: found before anything else of their name.
    linked: usize,
    /// Source files a built program carries.
    files: &'static [(&'static str, &'static str)],
}

fn fail(code: &str, args: &[&str]) -> (String, Vec<String>) {
    (code.to_string(), args.iter().map(|a| a.to_string()).collect())
}

impl Resolver {
    pub fn new(rt: Runtime, extra: Vec<Bundled>, files: &'static [(&'static str, &'static str)]) -> Resolver {
        let linked = extra.len();
        let mut bundled = extra;
        bundled.extend(self::bundled());
        Resolver {
            rt: RefCell::new(rt),
            bundled,
            found: RefCell::new(HashMap::new()),
            loaded: RefCell::new(HashMap::new()),
            project: manifest::find_project(Path::new(".")),
            linked,
            files,
        }
    }

    /// The folder of a package, or the failure of the whole import.
    fn locate(&self, name: &str) -> Result<Option<PathBuf>, (String, Vec<String>)> {
        locate(self.project.as_deref(), name)
    }

    /// A native library, loaded once.
    fn load_native(&self, name: &str, dir: &Path, m: &Manifest) -> Result<usize, (String, Vec<String>)> {
        let Some(id) = m.native_id() else {
            return Err(fail("ImportError.ImportDLLNotFound", &[name]));
        };
        let platform = manifest::platform();
        let native = m.native.as_ref().unwrap();
        let path = match native.files.get(&platform) {
            Some(f) => dir.join(&f.file),
            None => manifest::built_native(dir, &id),
        };
        if !path.is_file() {
            return Err(fail("ImportError.ImportNativeMissing", &[name, &platform]));
        }
        let key = std::path::absolute(&path).unwrap_or(path.clone());
        if let Some(r) = self.loaded.borrow().get(&key) {
            return r.clone();
        }
        let r = self
            .rt
            .borrow_mut()
            .load_dynamic(&path, &id)
            .map_err(|_| fail("ImportError.ImportNativeLoadFailed", &[&path.display().to_string()]));
        self.loaded.borrow_mut().insert(key, r.clone());
        r
    }

    fn package_dir(&self, name: &str, dir: &Path) -> Package {
        let m = match Manifest::load(dir) {
            Ok(m) => m.unwrap_or_default(),
            Err(e) => {
                return Package {
                    fail: Some(fail("ImportError.ImportManifestInvalid", &[name, &e])),
                    unsupported: false,
                    native: Err(fail("ImportError.ImportDLLNotFound", &[name])),
                    core: false,
                    entries: Vec::new(),
                };
            }
        };
        let mut entries = Vec::new();
        for lang in ["hari", "kanade"] {
            let path = dir.join(m.entry(lang));
            if path.is_file() {
                entries.push(Entry { lang, path: path.display().to_string(), text: std::fs::read_to_string(&path).ok() });
            }
        }
        Package { fail: None, unsupported: false, native: self.load_native(name, dir, &m), core: false, entries }
    }

    fn bundled_package(&self, b: &Bundled) -> Package {
        let mut entries = Vec::new();
        for (lang, text, file) in [("hari", b.hari, "hari/index.hr"), ("kanade", b.kanade, "kanade/index.knd")] {
            if let Some(t) = text {
                entries.push(Entry { lang, path: format!("<haru>/packages/{}/{file}", b.name), text: Some(t.to_string()) });
            }
        }
        let native = match b.native {
            None => Err(fail("ImportError.ImportDLLNotFound", &[b.name])),
            Some(entry) => {
                let mut rt = self.rt.borrow_mut();
                let module = match rt.module_by_id(b.name) {
                    Some(m) => Ok(m),
                    None => rt.load_static(entry).map_err(|_| fail("ImportError.ImportNativeLoadFailed", &[b.name])),
                };
                module
            }
        };
        Package { fail: None, unsupported: b.unsupported, native, core: false, entries }
    }

    fn resolve(&self, lang: &str, name: &str) -> Option<Package> {
        if let Some(b) = self.bundled[..self.linked].iter().find(|b| b.name == name) {
            return Some(self.bundled_package(b));
        }
        match self.locate(name) {
            Err(f) => {
                return Some(Package {
                    fail: Some(f),
                    unsupported: false,
                    native: Err(fail("ImportError.ImportDLLNotFound", &[name])),
                    core: false,
                    entries: Vec::new(),
                })
            }
            Ok(Some(dir)) => return Some(self.package_dir(name, &dir)),
            Ok(None) => {}
        }
        if let Some(b) = self.bundled.iter().find(|b| b.name == name) {
            return Some(self.bundled_package(b));
        }
        let m = self.rt.borrow().module(lang, name)?;
        Some(Package { fail: None, unsupported: false, native: Ok(m), core: true, entries: Vec::new() })
    }
}

/// The folder of the package `name` for the project in `project`, or the
/// failure of the whole import (see the module's comment for the order).
fn locate(project: Option<&Path>, name: &str) -> Result<Option<PathBuf>, (String, Vec<String>)> {
    if manifest::is_package_path(name) {
        let not_installed = || fail("ImportError.ImportPackageNotInstalled", &[name]);
        let project = project.ok_or_else(not_installed)?;
        let m = Manifest::load(project).ok().flatten().unwrap_or_default();
        if let Some(dir) = m.replace.get(name) {
            return Ok(Some(project.join(dir)));
        }
        let lock = manifest::load_lock(project).map_err(|_| not_installed())?;
        let e = lock.get(name).ok_or_else(not_installed)?;
        let dir = manifest::cache_dir(name, &e.version).ok_or_else(not_installed)?;
        return if dir.is_dir() { Ok(Some(dir)) } else { Err(not_installed()) };
    }
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']) {
        return Ok(None);
    }
    let mut roots = vec![PathBuf::from("packages")];
    if let Some(extra) = std::env::var_os("HARU_PACKAGES") {
        roots.extend(std::env::split_paths(&extra).filter(|p| !p.as_os_str().is_empty()));
    }
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.join("packages"))) {
        roots.push(dir);
    }
    Ok(roots.into_iter().map(|r| r.join(name)).find(|d| d.is_dir()))
}

/// The folder `haru run` would use for a package name (for `haru build`).
pub fn locate_folder(name: &str) -> Result<Option<PathBuf>, String> {
    locate(manifest::find_project(Path::new(".")).as_deref(), name).map_err(|(code, args)| format!("{code} {}", args.join(" ")))
}

impl Packages for Resolver {
    fn find(&self, lang: &str, name: &str) -> Option<Package> {
        let key = (lang.to_string(), name.to_string());
        if let Some(p) = self.found.borrow().get(&key) {
            return p.clone();
        }
        let p = self.resolve(lang, name);
        self.found.borrow_mut().insert(key, p.clone());
        p
    }

    fn names(&self, module: usize, lang: &str) -> Vec<String> {
        self.rt.borrow().function_names(module, lang)
    }

    fn read_file(&self, path: &str) -> Option<String> {
        match self.files.iter().find(|(p, _)| *p == path) {
            Some((_, text)) => Some(text.to_string()),
            None => std::fs::read_to_string(path).ok(),
        }
    }
}
