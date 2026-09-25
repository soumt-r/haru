//! Opening dynamic libraries without dependencies. Libraries are never closed:
//! module descriptors point into them.

use std::path::Path;

use haru_abi::EntryFn;

#[cfg(windows)]
pub(crate) fn open(path: &Path, symbol: &str) -> Result<EntryFn, String> {
    use std::ffi::{c_void, CString};
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryW(name: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const std::ffi::c_char) -> *mut c_void;
    }

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let lib = unsafe { LoadLibraryW(wide.as_ptr()) };
    if lib.is_null() {
        return Err(format!("{}: {}", path.display(), std::io::Error::last_os_error()));
    }
    let name = CString::new(symbol).map_err(|e| e.to_string())?;
    let sym = unsafe { GetProcAddress(lib, name.as_ptr()) };
    if sym.is_null() {
        return Err(format!("{}: no symbol {symbol}", path.display()));
    }
    Ok(unsafe { std::mem::transmute::<*mut c_void, EntryFn>(sym) })
}

#[cfg(unix)]
pub(crate) fn open(path: &Path, symbol: &str) -> Result<EntryFn, String> {
    use std::ffi::{c_char, c_int, c_void, CStr, CString};
    use std::os::unix::ffi::OsStrExt;

    #[cfg_attr(target_os = "linux", link(name = "dl"))]
    extern "C" {
        fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        fn dlerror() -> *const c_char;
    }
    const RTLD_NOW: c_int = 2;

    let last_error = || unsafe {
        let e = dlerror();
        if e.is_null() { "unknown error".to_string() } else { CStr::from_ptr(e).to_string_lossy().into_owned() }
    };
    let cpath = CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let lib = unsafe { dlopen(cpath.as_ptr(), RTLD_NOW) };
    if lib.is_null() {
        return Err(last_error());
    }
    let name = CString::new(symbol).map_err(|e| e.to_string())?;
    let sym = unsafe { dlsym(lib, name.as_ptr()) };
    if sym.is_null() {
        return Err(format!("{}: no symbol {symbol}", path.display()));
    }
    Ok(unsafe { std::mem::transmute::<*mut c_void, EntryFn>(sym) })
}

#[cfg(not(any(windows, unix)))]
pub(crate) fn open(_path: &Path, _symbol: &str) -> Result<EntryFn, String> {
    Err("dynamic modules are not supported on this platform".into())
}

/// The file name a dynamic library gets on this platform (`greet.dll`,
/// `libgreet.so`, `libgreet.dylib`).
pub fn library_file_name(name: &str) -> String {
    format!("{}{}{}", std::env::consts::DLL_PREFIX, name, std::env::consts::DLL_SUFFIX)
}
