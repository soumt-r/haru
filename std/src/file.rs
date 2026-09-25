//! [파일] / 【ファイル】, as Hana implements it (`std/stdimpl/file.go`).
//! What the operating system says becomes one of Hana's wordings (the OS
//! text is in the OS's language, not the script's).

use std::fs;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use haru_sdk::prelude::*;

use crate::hana::{exactly, new_list, string};

haru_sdk::entry!(pub(crate) fn entry = "file", build);

fn build(m: &mut Module) {
    crate::describe(m, "file", &[
        ("file.read", read),
        ("file.lines", lines),
        ("file.write", |a| write(a, false)),
        ("file.append", |a| write(a, true)),
        ("file.exists", exists),
        ("file.isdir", is_dir),
        ("file.delete", delete),
        ("file.list", list),
        ("file.mkdir", mkdir),
        ("file.move", move_to),
    ]);
}

static DENIED: AtomicBool = AtomicBool::new(false);

/// Turns every file operation off (`haru run --allow-file=false`).
pub fn deny_files() {
    DENIED.store(true, Ordering::Relaxed);
}

fn error(path: &str, e: io::Error) -> Error {
    let code = match e.kind() {
        io::ErrorKind::NotFound => "FileError.FileNotFound",
        io::ErrorKind::PermissionDenied => "FileError.FileAccessDenied",
        _ => "FileError.FileFailed",
    };
    Error::new(code).arg(path)
}

/// The path argument, once the access policy allows it.
fn path(args: &[Value], i: usize) -> Result<String> {
    let p = string(args, i)?.to_string();
    if DENIED.load(Ordering::Relaxed) {
        return Err(Error::new("FileError.FileBlocked"));
    }
    Ok(p)
}

fn is_directory(path: &str) -> Result<()> {
    Err(Error::new("FileError.FileIsDirectory").arg(path))
}

fn read_text(path: &str) -> Result<String> {
    let meta = fs::metadata(path).map_err(|e| error(path, e))?;
    if meta.is_dir() {
        is_directory(path)?;
    }
    let data = fs::read(path).map_err(|e| error(path, e))?;
    Ok(String::from_utf8(data).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()))
}

fn read(args: &[Value]) -> Result<Value> {
    exactly(args, 1)?;
    let p = path(args, 0)?;
    Ok(Value::str(&read_text(&p)?))
}

/// The lines without their line breaks (and no empty last line when the file
/// ends with one).
fn lines(args: &[Value]) -> Result<Value> {
    exactly(args, 1)?;
    let p = path(args, 0)?;
    let text = read_text(&p)?;
    if text.is_empty() {
        return new_list([]);
    }
    let mut parts: Vec<&str> = text.split('\n').collect();
    if parts.last() == Some(&"") {
        parts.pop();
    }
    new_list(parts.into_iter().map(|l| Value::str(l.strip_suffix('\r').unwrap_or(l))))
}

fn write(args: &[Value], append: bool) -> Result<Value> {
    exactly(args, 2)?;
    let p = path(args, 0)?;
    let text = string(args, 1)?;
    if fs::metadata(&p).is_ok_and(|m| m.is_dir()) {
        is_directory(&p)?;
    }
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .append(append)
        .truncate(!append)
        .open(&p)
        .map_err(|e| error(&p, e))?;
    f.write_all(text.as_bytes()).map_err(|e| error(&p, e))?;
    Ok(Value::NULL)
}

fn exists(args: &[Value]) -> Result<Value> {
    exactly(args, 1)?;
    let p = path(args, 0)?;
    Ok(Value::bool(fs::metadata(p).is_ok()))
}

fn is_dir(args: &[Value]) -> Result<Value> {
    exactly(args, 1)?;
    let p = path(args, 0)?;
    Ok(Value::bool(fs::metadata(p).is_ok_and(|m| m.is_dir())))
}

/// A file or an empty folder, the way Go's `os.Remove` does it.
fn delete(args: &[Value]) -> Result<Value> {
    exactly(args, 1)?;
    let p = path(args, 0)?;
    let file_err = match fs::remove_file(&p) {
        Ok(()) => return Ok(Value::NULL),
        Err(e) => e,
    };
    let dir_err = match fs::remove_dir(&p) {
        Ok(()) => return Ok(Value::NULL),
        Err(e) => e,
    };
    let e = match fs::symlink_metadata(&p) {
        Err(e) => e,
        Ok(m) if m.is_dir() => dir_err,
        Ok(m) if m.permissions().readonly() => {
            // Go clears the read-only mark and tries again.
            let mut perm = m.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perm.set_readonly(false);
            match fs::set_permissions(&p, perm).and_then(|()| fs::remove_file(&p)) {
                Ok(()) => return Ok(Value::NULL),
                Err(e) => e,
            }
        }
        Ok(_) => file_err,
    };
    Err(error(&p, e))
}

/// The names inside a folder, sorted.
fn list(args: &[Value]) -> Result<Value> {
    exactly(args, 1)?;
    let p = path(args, 0)?;
    let entries = fs::read_dir(&p).map_err(|e| error(&p, e))?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| error(&p, e))?;
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    new_list(names.iter().map(|n| Value::str(n)))
}

/// A folder and any missing folders above it; an existing folder is fine.
fn mkdir(args: &[Value]) -> Result<Value> {
    exactly(args, 1)?;
    let p = path(args, 0)?;
    mkdir_all(&p).map_err(|e| error(&p, e))?;
    Ok(Value::NULL)
}

/// Go's `os.MkdirAll`, whose error for a file in the way is Go's ENOTDIR
/// (on Windows Go counts it as "not found").
fn mkdir_all(path: &str) -> io::Result<()> {
    let not_dir = || {
        io::Error::from(if cfg!(windows) { io::ErrorKind::NotFound } else { io::ErrorKind::NotADirectory })
    };
    if let Ok(m) = fs::metadata(path) {
        return if m.is_dir() { Ok(()) } else { Err(not_dir()) };
    }
    let sep = |c: char| c == '/' || (cfg!(windows) && c == '\\');
    let trimmed = path.trim_end_matches(sep);
    let i = trimmed.rfind(sep).unwrap_or(0);
    let parent = &path[..i];
    if parent.len() > volume_len(path) {
        mkdir_all(parent)?;
    }
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(e) => match fs::symlink_metadata(path) {
            Ok(m) if m.is_dir() => Ok(()),
            _ => Err(e),
        },
    }
}

/// The length of a leading "C:" (Go's `filepath.VolumeName`, drive letters only).
fn volume_len(path: &str) -> usize {
    let b = path.as_bytes();
    if cfg!(windows) && b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        2
    } else {
        0
    }
}

/// Renames or moves; an existing file at the destination is replaced.
fn move_to(args: &[Value]) -> Result<Value> {
    exactly(args, 2)?;
    let from = path(args, 0)?;
    let to = path(args, 1)?;
    fs::rename(&from, &to).map_err(|e| error(&from, e))?;
    Ok(Value::NULL)
}
