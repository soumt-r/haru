//! Haru native module ABI, version 1.
//!
//! This crate is the single definition of every type that crosses the boundary
//! between the Haru runtime (the host) and a native module. Everything here is
//! `#[repr(C)]`, so a module can be written in any language that can export a
//! C function; Rust authors use `haru-sdk` on top of it.
//!
//! A module exports one function:
//!
//! ```text
//! const ModuleDesc* haru_module_v1_<id>(const HostApi* host);
//! ```
//!
//! and the host calls each function of the descriptor as a [`NativeFn`]. The
//! arguments are a pointer straight into the VM's registers: [`RawValue`] is the
//! VM's own value layout, so a call converts and copies nothing.
#![no_std]

use core::ffi::c_void;

pub const ABI_VERSION: u32 = 1;

/// Prefix of the symbol a dynamic library exports; the module id follows it
/// (`haru_module_v1_greet`), so several modules can be linked into one binary.
pub const ENTRY_PREFIX: &str = "haru_module_v1_";

/// What a [`RawValue`] holds. Tags from [`tag::STR`] on point at host-owned,
/// reference-counted objects.
pub mod tag {
    pub const NULL: u32 = 0;
    pub const BOOL: u32 = 1;
    pub const NUM: u32 = 2;
    pub const STR: u32 = 3;
    pub const LIST: u32 = 4;
    pub const DICT: u32 = 5;
    pub const FUNC: u32 = 6;
    pub const OBJECT: u32 = 7;
    pub const RESOURCE: u32 = 8;

    /// Whether values with this tag own a reference that must be retained and
    /// released.
    #[inline]
    pub const fn is_heap(tag: u32) -> bool {
        tag >= STR
    }
}

/// The kind a parameter accepts. The host checks every argument against it
/// before the call, so a function never sees a value of the wrong kind.
pub mod kind {
    pub const ANY: u32 = 0;
    pub const NUM: u32 = 1;
    pub const STR: u32 = 2;
    pub const BOOL: u32 = 3;
    pub const LIST: u32 = 4;
    pub const DICT: u32 = 5;
    pub const FUNC: u32 = 6;
    /// As the only parameter: any number of arguments of any kind, which the
    /// function checks itself (the standard library, to report Hana's errors).
    pub const REST: u32 = 99;

    /// Whether a value with `tag` is accepted by a parameter of `kind`.
    #[inline]
    pub const fn accepts(kind: u32, tag: u32) -> bool {
        match kind {
            ANY => true,
            NUM => tag == super::tag::NUM,
            STR => tag == super::tag::STR,
            BOOL => tag == super::tag::BOOL,
            LIST => tag == super::tag::LIST,
            DICT => tag == super::tag::DICT,
            FUNC => tag == super::tag::FUNC,
            _ => false,
        }
    }
}

/// A value, 16 bytes. `NULL`/`BOOL`/`NUM` keep their data in `payload`
/// (a bool is 0 or 1, a number is the bits of an `f64`); heap tags keep a
/// pointer the host owns and only the host may dereference.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RawValue {
    pub tag: u32,
    pub pad: u32,
    pub payload: u64,
}

impl RawValue {
    pub const NULL: RawValue = RawValue { tag: tag::NULL, pad: 0, payload: 0 };

    #[inline]
    pub const fn bool(b: bool) -> RawValue {
        RawValue { tag: tag::BOOL, pad: 0, payload: b as u64 }
    }

    #[inline]
    pub fn num(n: f64) -> RawValue {
        RawValue { tag: tag::NUM, pad: 0, payload: n.to_bits() }
    }

    #[inline]
    pub fn as_num(self) -> Option<f64> {
        (self.tag == tag::NUM).then(|| f64::from_bits(self.payload))
    }

    #[inline]
    pub fn as_bool(self) -> Option<bool> {
        (self.tag == tag::BOOL).then_some(self.payload != 0)
    }
}

/// A borrowed UTF-8 string: `ptr` is valid for `len` bytes for as long as the
/// thing it was taken from.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Str {
    pub ptr: *const u8,
    pub len: usize,
}

impl Str {
    pub const EMPTY: Str = Str { ptr: core::ptr::null(), len: 0 };

    #[inline]
    pub const fn new(s: &str) -> Str {
        Str { ptr: s.as_ptr(), len: s.len() }
    }

    /// # Safety
    /// The bytes must be valid UTF-8 and outlive `'a`.
    #[inline]
    pub unsafe fn as_str<'a>(self) -> &'a str {
        if self.len == 0 {
            return "";
        }
        core::str::from_utf8_unchecked(core::slice::from_raw_parts(self.ptr, self.len))
    }
}

/// Returned by native functions and by host functions that can fail.
pub type Status = u32;
pub const STATUS_OK: Status = 0;
/// An error is pending on the call context (set with [`HostApi::throw`]).
pub const STATUS_ERROR: Status = 1;

/// The host's state for one native call. Opaque to modules.
#[repr(C)]
pub struct HostCtx {
    _private: [u8; 0],
}

/// A native function.
///
/// `args` are borrowed for the duration of the call; their count and kinds
/// already match the descriptor. On success the function writes an owned value
/// (+1 reference) to `out` and returns [`STATUS_OK`]; on failure it calls one of
/// the host's `throw` functions and returns what it returned.
pub type NativeFn = unsafe extern "C" fn(
    userdata: *const c_void,
    ctx: *mut HostCtx,
    args: *const RawValue,
    argc: usize,
    out: *mut RawValue,
) -> Status;

/// A name in one language (`"hari"`, `"kanade"`).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Name {
    pub lang: Str,
    pub name: Str,
}

#[repr(C)]
pub struct FunctionDesc {
    /// Language-neutral id, unique in the module (`"ceil"`).
    pub id: Str,
    pub names: *const Name,
    pub names_len: usize,
    /// One [`kind`] per parameter.
    pub params: *const u32,
    pub params_len: usize,
    /// How many leading parameters must be given; the rest may be left out.
    pub required: usize,
    pub func: NativeFn,
    pub userdata: *const c_void,
}

/// A message template for an error code in one language. `{0}`, `{1}`, ... are
/// replaced with the error's arguments.
#[repr(C)]
pub struct MessageDesc {
    pub code: Str,
    pub lang: Str,
    pub template: Str,
}

#[repr(C)]
pub struct ModuleDesc {
    pub abi_version: u32,
    /// Language-neutral id (`"math"`).
    pub id: Str,
    pub names: *const Name,
    pub names_len: usize,
    pub functions: *const FunctionDesc,
    pub functions_len: usize,
    pub messages: *const MessageDesc,
    pub messages_len: usize,
}

/// The module entry point.
pub type EntryFn = unsafe extern "C" fn(host: *const HostApi) -> *const ModuleDesc;

/// Functions the host gives to modules. Later versions only append fields;
/// `size` tells a module which ones exist.
#[repr(C)]
pub struct HostApi {
    pub abi_version: u32,
    pub size: u32,

    /// Add / drop one reference. No-ops for non-heap tags.
    pub retain: unsafe extern "C" fn(v: RawValue),
    pub release: unsafe extern "C" fn(v: RawValue),

    /// The text of a string value, valid while the value is alive.
    pub str_view: unsafe extern "C" fn(v: RawValue) -> Str,
    /// A new string (+1) holding a copy of `s`.
    pub str_new: unsafe extern "C" fn(s: Str) -> RawValue,

    /// A new, empty list (+1).
    pub list_new: unsafe extern "C" fn(capacity: usize) -> RawValue,
    pub list_len: unsafe extern "C" fn(list: RawValue) -> usize,
    /// The item at a 0-based `index` (+1), or null when out of range.
    pub list_get: unsafe extern "C" fn(list: RawValue, index: usize) -> RawValue,
    /// Appends `item`, taking over its reference.
    pub list_push: unsafe extern "C" fn(list: RawValue, item: RawValue),

    /// Calls a function value. On success `out` receives the result (+1).
    /// On failure the error is already pending on `ctx`: return the status.
    pub call: unsafe extern "C" fn(
        ctx: *mut HostCtx,
        func: RawValue,
        args: *const RawValue,
        argc: usize,
        out: *mut RawValue,
    ) -> Status,

    /// Raises the calling module's error `code` with arguments (borrowed; the
    /// host keeps its own references) and returns [`STATUS_ERROR`].
    pub throw: unsafe extern "C" fn(
        ctx: *mut HostCtx,
        code: Str,
        args: *const RawValue,
        argc: usize,
    ) -> Status,
    /// Raises the host's argument-type error for the 0-based parameter `index`.
    pub throw_type: unsafe extern "C" fn(ctx: *mut HostCtx, index: usize, expected: u32) -> Status,
}

// Descriptors are built once and only read afterwards.
unsafe impl Sync for ModuleDesc {}
unsafe impl Sync for FunctionDesc {}
unsafe impl Sync for HostApi {}

const _: () = assert!(core::mem::size_of::<RawValue>() == 16);
