//! Write Haru modules in plain Rust.
//!
//! ```ignore
//! use haru_sdk::prelude::*;
//!
//! fn build(m: &mut Module) {
//!     m.name("hari", "인사").name("kanade", "挨拶");
//!     m.func("hello", |who: Str| format!("안녕, {}!", &*who))
//!         .name("hari", "인사말")
//!         .name("kanade", "挨拶文");
//! }
//!
//! haru_sdk::export!("greet", build);
//! ```
//!
//! The same crate works as a dynamic library (`crate-type = ["cdylib"]`, the
//! host finds `haru_module_v1_greet`) and linked into a Haru binary (call
//! `greet::haru_entry`). Arguments are converted from the parameter types of the
//! closure; the host has already checked their count and kinds.

use std::cell::Cell;
use std::ffi::c_void;
use std::ops::Deref;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicPtr, Ordering};

pub use haru_abi as abi;
use haru_abi::{kind, tag, HostApi, HostCtx, ModuleDesc, RawValue, Status, STATUS_ERROR, STATUS_OK};

mod module;
pub use module::{FuncEntry, Module, ResourceEntry};

pub mod prelude {
    pub use crate::{Dict, Error, Func, List, Module, Res, Result, Str, Value};
}

static HOST: AtomicPtr<HostApi> = AtomicPtr::new(std::ptr::null_mut());

thread_local! {
    static CTX: Cell<*mut HostCtx> = const { Cell::new(std::ptr::null_mut()) };
}

fn host() -> &'static HostApi {
    let p = HOST.load(Ordering::Acquire);
    assert!(!p.is_null(), "haru-sdk: module used before the host loaded it");
    unsafe { &*p }
}

/// Builds the module and hands its descriptor to the host. Called by
/// [`export!`] and [`entry!`].
#[doc(hidden)]
pub unsafe fn __entry(host: *const HostApi, id: &str, build: fn(&mut Module)) -> *const ModuleDesc {
    if host.is_null() || (*host).abi_version != abi::ABI_VERSION {
        return std::ptr::null();
    }
    HOST.store(host as *mut HostApi, Ordering::Release);
    // One descriptor per module in a process: a second load gets the first
    // (its resource kinds must stay the ones values point at).
    static BUILT: std::sync::Mutex<Vec<(String, usize)>> = std::sync::Mutex::new(Vec::new());
    let mut built = BUILT.lock().unwrap();
    if let Some((_, d)) = built.iter().find(|(i, _)| i == id) {
        return *d as *const ModuleDesc;
    }
    let mut m = Module::new(id);
    build(&mut m);
    let desc = m.leak();
    built.push((id.to_string(), desc as usize));
    desc
}

/// Exports a module from a crate. The function is `pub haru_entry` for static
/// linking and the symbol `haru_module_v1_<id>` for dynamic loading.
#[macro_export]
macro_rules! export {
    ($id:literal, $build:path) => {
        #[export_name = concat!("haru_module_v1_", $id)]
        pub unsafe extern "C" fn haru_entry(
            host: *const $crate::abi::HostApi,
        ) -> *const $crate::abi::ModuleDesc {
            $crate::__entry(host, $id, $build)
        }
    };
}

/// Declares an entry function without exporting a symbol, for crates that hold
/// several statically linked modules (the standard library).
#[macro_export]
macro_rules! entry {
    ($vis:vis fn $name:ident = $id:literal, $build:path) => {
        $vis unsafe extern "C" fn $name(
            host: *const $crate::abi::HostApi,
        ) -> *const $crate::abi::ModuleDesc {
            $crate::__entry(host, $id, $build)
        }
    };
}

// ---------------------------------------------------------------------------
// Values

/// Any value, holding its own reference.
#[repr(transparent)]
pub struct Value(RawValue);

impl Value {
    pub const NULL: Value = Value(RawValue::NULL);

    pub fn num(n: f64) -> Value {
        Value(RawValue::num(n))
    }

    pub fn bool(b: bool) -> Value {
        Value(RawValue::bool(b))
    }

    pub fn str(s: &str) -> Value {
        Value(unsafe { (host().str_new)(abi::Str::new(s)) })
    }

    /// Takes a new reference to a borrowed value.
    fn borrowed(raw: RawValue) -> Value {
        if tag::is_heap(raw.tag) {
            unsafe { (host().retain)(raw) }
        }
        Value(raw)
    }

    fn into_raw(self) -> RawValue {
        let raw = self.0;
        std::mem::forget(self);
        raw
    }

    pub fn raw(&self) -> RawValue {
        self.0
    }

    pub fn as_num(&self) -> Option<f64> {
        self.0.as_num()
    }

    pub fn as_bool(&self) -> Option<bool> {
        self.0.as_bool()
    }

    pub fn is_null(&self) -> bool {
        self.0.tag == tag::NULL
    }

    pub fn as_str(&self) -> Option<Str> {
        (self.0.tag == tag::STR).then(|| Str(self.clone()))
    }

    pub fn as_list(&self) -> Option<List> {
        (self.0.tag == tag::LIST).then(|| List(self.clone()))
    }

    pub fn as_dict(&self) -> Option<Dict> {
        (self.0.tag == tag::DICT).then(|| Dict(self.clone()))
    }

    pub fn tag(&self) -> u32 {
        self.0.tag
    }
}

impl Clone for Value {
    fn clone(&self) -> Value {
        Value::borrowed(self.0)
    }
}

impl Drop for Value {
    fn drop(&mut self) {
        if tag::is_heap(self.0.tag) {
            unsafe { (host().release)(self.0) }
        }
    }
}

/// A string owned by the host. Reads as `&str` without copying.
#[derive(Clone)]
pub struct Str(Value);

impl Deref for Str {
    type Target = str;
    fn deref(&self) -> &str {
        // The host keeps the text alive and unchanged while we hold a reference.
        unsafe { (host().str_view)(self.0 .0).as_str() }
    }
}

impl std::fmt::Display for Str {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self)
    }
}

/// A list shared with the program: changes are visible to it.
#[derive(Clone)]
pub struct List(Value);

impl List {
    pub fn new() -> List {
        List(Value(unsafe { (host().list_new)(0) }))
    }

    pub fn len(&self) -> usize {
        unsafe { (host().list_len)(self.0 .0) }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The item at a 0-based index.
    pub fn get(&self, index: usize) -> Option<Value> {
        (index < self.len()).then(|| Value(unsafe { (host().list_get)(self.0 .0, index) }))
    }

    pub fn push(&self, item: impl IntoRet) -> Result<()> {
        let v = item.into_ret()?;
        unsafe { (host().list_push)(self.0 .0, v.into_raw()) };
        Ok(())
    }

    pub fn iter(&self) -> impl Iterator<Item = Value> + '_ {
        (0..self.len()).filter_map(|i| self.get(i))
    }

    /// Replaces the item at a 0-based index; false when out of range.
    pub fn set(&self, index: usize, item: impl IntoRet) -> Result<bool> {
        let v = item.into_ret()?;
        Ok(unsafe { (host().list_set)(self.0 .0, index, v.into_raw()) })
    }
}

/// A dictionary shared with the program. Keys compare as the program
/// compares them (a number key is not the same as its text).
#[derive(Clone)]
pub struct Dict(Value);

impl Dict {
    pub fn new() -> Dict {
        Dict(Value(unsafe { (host().dict_new)() }))
    }

    pub fn len(&self) -> usize {
        unsafe { (host().dict_len)(self.0 .0) }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, key: &Value) -> Option<Value> {
        let mut out = RawValue::NULL;
        unsafe { (host().dict_get)(self.0 .0, key.0, &mut out) }.then(|| Value(out))
    }

    pub fn set(&self, key: impl IntoRet, value: impl IntoRet) -> Result<()> {
        let (k, v) = (key.into_ret()?, value.into_ret()?);
        unsafe { (host().dict_set)(self.0 .0, k.into_raw(), v.into_raw()) };
        Ok(())
    }

    /// The keys, in no particular order.
    pub fn keys(&self) -> List {
        List(Value(unsafe { (host().dict_keys)(self.0 .0) }))
    }
}

impl Default for Dict {
    fn default() -> Dict {
        Dict::new()
    }
}

impl Default for List {
    fn default() -> List {
        List::new()
    }
}

/// A function value from the program (or another module).
#[derive(Clone)]
pub struct Func(Value);

impl Func {
    /// Calls the function on the current thread. Only valid while the host is
    /// calling into this module.
    pub fn call(&self, args: &[Value]) -> Result<Value> {
        self.0.call(args)
    }
}

impl Value {
    /// Calls the value as a function and catches what it throws: the error's
    /// message (without its kind) in a locale — 0 English, 1 Korean, 2
    /// Japanese. The error is then handled, as a program's `일단 해보자` would.
    pub fn call_catching(&self, args: &[Value], locale: u32) -> std::result::Result<Value, String> {
        match self.call(args) {
            Ok(v) => Ok(v),
            Err(Error(ErrorKind::Pending)) => {
                let ctx = CTX.with(|c| c.get());
                let mut out = RawValue::NULL;
                if unsafe { (host().take_error)(ctx, locale, &mut out) } {
                    let v = Value(out);
                    Err(v.as_str().map(|s| s.to_string()).unwrap_or_default())
                } else {
                    Err(String::new())
                }
            }
            Err(_) => Err(String::new()),
        }
    }

    /// Calls the value as a function (the host fails with `NotCallable` when
    /// it is not one). Only valid while the host is calling into this module.
    pub fn call(&self, args: &[Value]) -> Result<Value> {
        let ctx = CTX.with(|c| c.get());
        assert!(!ctx.is_null(), "haru-sdk: Func::call outside of a native call");
        let mut out = RawValue::NULL;
        // `Value` is a transparent wrapper over `RawValue` in memory.
        let status = unsafe {
            (host().call)(ctx, self.0, args.as_ptr() as *const RawValue, args.len(), &mut out)
        };
        if status == STATUS_OK {
            Ok(Value(out))
        } else {
            Err(Error(ErrorKind::Pending))
        }
    }
}

/// Writes out what the program has printed so far. Call it before waiting
/// (a sleep, the network), so the output does not sit in a buffer meanwhile.
pub fn flush_output() {
    let ctx = CTX.with(|c| c.get());
    if !ctx.is_null() {
        unsafe { (host().flush)(ctx) }
    }
}

// ---------------------------------------------------------------------------
// Resources

/// The resource kind described for each Rust type, in this library.
static KINDS: std::sync::Mutex<Vec<(std::any::TypeId, usize)>> = std::sync::Mutex::new(Vec::new());

pub(crate) fn register_kind(t: std::any::TypeId, desc: &'static abi::ResourceDesc) {
    KINDS.lock().unwrap().push((t, desc as *const _ as usize));
}

fn kind_of<T: 'static>() -> Option<*const abi::ResourceDesc> {
    let t = std::any::TypeId::of::<T>();
    KINDS.lock().unwrap().iter().find(|(k, _)| *k == t).map(|(_, d)| *d as *const abi::ResourceDesc)
}

/// A resource of kind `T` (see [`Module::resource`]): the program's value,
/// read as the `T` inside. Use `Cell`/`RefCell` in `T` for what changes.
pub struct Res<T: 'static> {
    value: Value,
    ptr: *const T,
}

impl<T: 'static> Res<T> {
    /// A new resource holding `obj`. The kind must be described by this
    /// module (`m.resource::<T>(...)`).
    pub fn new(obj: T) -> Result<Res<T>> {
        let kind = kind_of::<T>().ok_or_else(|| Error::new("UnknownResource"))?;
        let ptr = Box::into_raw(Box::new(obj));
        let raw = unsafe { (host().resource_new)(kind, ptr as *mut c_void) };
        Ok(Res { value: Value(raw), ptr })
    }

    /// The program's value of it.
    pub fn value(&self) -> &Value {
        &self.value
    }
}

impl<T: 'static> Deref for Res<T> {
    type Target = T;
    fn deref(&self) -> &T {
        // Alive while `value` holds the resource.
        unsafe { &*self.ptr }
    }
}

impl<T: 'static> Clone for Res<T> {
    fn clone(&self) -> Res<T> {
        Res { value: self.value.clone(), ptr: self.ptr }
    }
}

impl Value {
    /// This value as a resource of kind `T`.
    pub fn as_res<T: 'static>(&self) -> Option<Res<T>> {
        let kind = kind_of::<T>()?;
        let ptr = unsafe { (host().resource_get)(self.0, kind) } as *const T;
        (!ptr.is_null()).then(|| Res { value: self.clone(), ptr })
    }
}

impl<T: 'static> FromArg for Res<T> {
    const KIND: u32 = kind::RESOURCE;
    fn from_arg(raw: RawValue) -> Option<Res<T>> {
        Value::borrowed(raw).as_res()
    }
}

impl<T: 'static> IntoRet for Res<T> {
    fn into_ret(self) -> Result<Value> {
        Ok(self.value)
    }
}

// ---------------------------------------------------------------------------
// Errors

/// A module error: a code (looked up in the module's messages) and arguments.
pub struct Error(ErrorKind);

enum ErrorKind {
    Code { code: String, args: Vec<Value> },
    /// The host already holds the error (a callback failed); pass it on.
    Pending,
}

impl Error {
    pub fn new(code: &str) -> Error {
        Error(ErrorKind::Code { code: code.to_string(), args: Vec::new() })
    }

    /// Adds an argument for the message template (`{0}`, `{1}`, ...).
    pub fn arg(mut self, v: impl IntoRet) -> Error {
        if let ErrorKind::Code { args, .. } = &mut self.0 {
            args.push(v.into_ret().unwrap_or(Value::NULL));
        }
        self
    }
}

pub type Result<T> = std::result::Result<T, Error>;

// ---------------------------------------------------------------------------
// Conversions

/// A parameter type.
pub trait FromArg: Sized {
    const KIND: u32;
    /// `raw` is borrowed; `None` when its kind does not match.
    fn from_arg(raw: RawValue) -> Option<Self>;
}

impl FromArg for f64 {
    const KIND: u32 = kind::NUM;
    fn from_arg(raw: RawValue) -> Option<f64> {
        raw.as_num()
    }
}

impl FromArg for bool {
    const KIND: u32 = kind::BOOL;
    fn from_arg(raw: RawValue) -> Option<bool> {
        raw.as_bool()
    }
}

impl FromArg for Str {
    const KIND: u32 = kind::STR;
    fn from_arg(raw: RawValue) -> Option<Str> {
        (raw.tag == tag::STR).then(|| Str(Value::borrowed(raw)))
    }
}

impl FromArg for List {
    const KIND: u32 = kind::LIST;
    fn from_arg(raw: RawValue) -> Option<List> {
        (raw.tag == tag::LIST).then(|| List(Value::borrowed(raw)))
    }
}

impl FromArg for Dict {
    const KIND: u32 = kind::DICT;
    fn from_arg(raw: RawValue) -> Option<Dict> {
        (raw.tag == tag::DICT).then(|| Dict(Value::borrowed(raw)))
    }
}

impl FromArg for Func {
    const KIND: u32 = kind::FUNC;
    fn from_arg(raw: RawValue) -> Option<Func> {
        (raw.tag == tag::FUNC).then(|| Func(Value::borrowed(raw)))
    }
}

impl FromArg for Value {
    const KIND: u32 = kind::ANY;
    fn from_arg(raw: RawValue) -> Option<Value> {
        Some(Value::borrowed(raw))
    }
}

/// A return type.
pub trait IntoRet {
    fn into_ret(self) -> Result<Value>;
}

impl IntoRet for Value {
    fn into_ret(self) -> Result<Value> {
        Ok(self)
    }
}

impl IntoRet for () {
    fn into_ret(self) -> Result<Value> {
        Ok(Value::NULL)
    }
}

impl IntoRet for f64 {
    fn into_ret(self) -> Result<Value> {
        Ok(Value::num(self))
    }
}

impl IntoRet for bool {
    fn into_ret(self) -> Result<Value> {
        Ok(Value::bool(self))
    }
}

impl IntoRet for &str {
    fn into_ret(self) -> Result<Value> {
        Ok(Value::str(self))
    }
}

impl IntoRet for String {
    fn into_ret(self) -> Result<Value> {
        Ok(Value::str(&self))
    }
}

impl IntoRet for Str {
    fn into_ret(self) -> Result<Value> {
        Ok(self.0)
    }
}

impl IntoRet for List {
    fn into_ret(self) -> Result<Value> {
        Ok(self.0)
    }
}

impl IntoRet for Dict {
    fn into_ret(self) -> Result<Value> {
        Ok(self.0)
    }
}

impl IntoRet for Func {
    fn into_ret(self) -> Result<Value> {
        Ok(self.0)
    }
}

impl<T: IntoRet> IntoRet for Vec<T> {
    fn into_ret(self) -> Result<Value> {
        let list = List::new();
        for item in self {
            list.push(item)?;
        }
        Ok(list.0)
    }
}

impl<T: IntoRet> IntoRet for Option<T> {
    fn into_ret(self) -> Result<Value> {
        self.map_or(Ok(Value::NULL), IntoRet::into_ret)
    }
}

impl<T: IntoRet> IntoRet for Result<T> {
    fn into_ret(self) -> Result<Value> {
        self.and_then(IntoRet::into_ret)
    }
}

// ---------------------------------------------------------------------------
// Handlers: closures of 0..=8 typed parameters

#[doc(hidden)]
pub enum Fail {
    /// Parameter index and the kind it wanted.
    Type(usize, u32),
    Error(Error),
}

/// A Rust function usable as a native function. Implemented for closures and
/// `fn` items whose parameters are [`FromArg`] and whose result is [`IntoRet`].
pub trait Handler<Args>: 'static {
    fn kinds() -> Vec<u32>;
    #[doc(hidden)]
    fn invoke(&self, args: &[RawValue]) -> std::result::Result<Value, Fail>;
}

macro_rules! impl_handler {
    ($(($A:ident, $a:ident, $i:tt)),*) => {
        impl<F, R, $($A,)*> Handler<($($A,)*)> for F
        where
            F: Fn($($A),*) -> R + 'static,
            R: IntoRet,
            $($A: FromArg,)*
        {
            fn kinds() -> Vec<u32> {
                vec![$($A::KIND),*]
            }

            #[allow(unused_variables)]
            fn invoke(&self, args: &[RawValue]) -> std::result::Result<Value, Fail> {
                $(
                    let $a = args
                        .get($i)
                        .and_then(|raw| $A::from_arg(*raw))
                        .ok_or(Fail::Type($i, $A::KIND))?;
                )*
                (self)($($a),*).into_ret().map_err(Fail::Error)
            }
        }
    };
}

impl_handler!();
impl_handler!((A0, a0, 0));
impl_handler!((A0, a0, 0), (A1, a1, 1));
impl_handler!((A0, a0, 0), (A1, a1, 1), (A2, a2, 2));
impl_handler!((A0, a0, 0), (A1, a1, 1), (A2, a2, 2), (A3, a3, 3));
impl_handler!((A0, a0, 0), (A1, a1, 1), (A2, a2, 2), (A3, a3, 3), (A4, a4, 4));
impl_handler!((A0, a0, 0), (A1, a1, 1), (A2, a2, 2), (A3, a3, 3), (A4, a4, 4), (A5, a5, 5));
impl_handler!((A0, a0, 0), (A1, a1, 1), (A2, a2, 2), (A3, a3, 3), (A4, a4, 4), (A5, a5, 5), (A6, a6, 6));
impl_handler!((A0, a0, 0), (A1, a1, 1), (A2, a2, 2), (A3, a3, 3), (A4, a4, 4), (A5, a5, 5), (A6, a6, 6), (A7, a7, 7));

/// The C entry of every SDK function: one instance per handler type.
unsafe extern "C" fn shim<H: Handler<A>, A>(
    userdata: *const c_void,
    ctx: *mut HostCtx,
    args: *const RawValue,
    argc: usize,
    out: *mut RawValue,
) -> Status {
    let handler = &*(userdata as *const H);
    let args = if argc == 0 { &[][..] } else { std::slice::from_raw_parts(args, argc) };

    let prev = CTX.with(|c| c.replace(ctx));
    let result = catch_unwind(AssertUnwindSafe(|| handler.invoke(args)));
    CTX.with(|c| c.set(prev));

    let host = host();
    match result {
        Ok(Ok(v)) => {
            *out = v.into_raw();
            STATUS_OK
        }
        Ok(Err(Fail::Type(index, want))) => (host.throw_type)(ctx, index, want),
        Ok(Err(Fail::Error(Error(ErrorKind::Pending)))) => STATUS_ERROR,
        Ok(Err(Fail::Error(Error(ErrorKind::Code { code, args })))) => {
            (host.throw)(ctx, abi::Str::new(&code), args.as_ptr() as *const RawValue, args.len())
        }
        Err(_) => (host.throw)(ctx, abi::Str::new("Panic"), std::ptr::null(), 0),
    }
}

/// The C entry of a [`Module::raw`] function (`userdata` is the fn pointer).
pub(crate) unsafe extern "C" fn raw_shim(
    userdata: *const c_void,
    ctx: *mut HostCtx,
    args: *const RawValue,
    argc: usize,
    out: *mut RawValue,
) -> Status {
    let f: fn(&[Value]) -> Result<Value> = std::mem::transmute(userdata);
    // `Value` is a transparent wrapper over `RawValue`; the slice is borrowed.
    let args: &[Value] = if argc == 0 { &[] } else { std::slice::from_raw_parts(args as *const Value, argc) };
    let prev = CTX.with(|c| c.replace(ctx));
    let result = catch_unwind(AssertUnwindSafe(|| f(args)));
    CTX.with(|c| c.set(prev));
    let host = host();
    match result {
        Ok(Ok(v)) => {
            *out = v.into_raw();
            STATUS_OK
        }
        Ok(Err(Error(ErrorKind::Pending))) => STATUS_ERROR,
        Ok(Err(Error(ErrorKind::Code { code, args }))) => {
            (host.throw)(ctx, abi::Str::new(&code), args.as_ptr() as *const RawValue, args.len())
        }
        Err(_) => (host.throw)(ctx, abi::Str::new("Panic"), std::ptr::null(), 0),
    }
}

pub(crate) fn native_fn<H: Handler<A>, A>() -> abi::NativeFn {
    shim::<H, A>
}

const _: () = assert!(std::mem::size_of::<Value>() == std::mem::size_of::<RawValue>());
