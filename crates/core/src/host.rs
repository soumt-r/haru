//! The [`HostApi`] this runtime gives to modules.

use haru_abi::{HostApi, HostCtx, RawValue, Status, Str, ABI_VERSION, STATUS_ERROR, STATUS_OK};

use crate::error::RuntimeError;
use crate::modules::{type_error, Runtime};
use crate::value::{self, Value};

/// What a `*mut HostCtx` points at during a native call.
pub(crate) struct CallCtx<'a> {
    pub rt: &'a Runtime,
    pub module: usize,
    pub pending: Option<RuntimeError>,
}

unsafe fn ctx<'a>(p: *mut HostCtx) -> &'a mut CallCtx<'a> {
    &mut *(p as *mut CallCtx)
}

unsafe extern "C" fn retain(v: RawValue) {
    value::retain(v)
}

unsafe extern "C" fn release(v: RawValue) {
    value::release(v)
}

unsafe extern "C" fn str_view(v: RawValue) -> Str {
    // Borrow without touching the count: the caller holds a reference.
    let v = std::mem::ManuallyDrop::new(Value::from_raw(v));
    v.as_str().map_or(Str::EMPTY, Str::new)
}

unsafe extern "C" fn str_new(s: Str) -> RawValue {
    Value::str(s.as_str()).into_raw()
}

unsafe extern "C" fn list_new(capacity: usize) -> RawValue {
    Value::list(Vec::with_capacity(capacity)).into_raw()
}

unsafe extern "C" fn list_len(list: RawValue) -> usize {
    let v = std::mem::ManuallyDrop::new(Value::from_raw(list));
    v.as_list().map_or(0, |l| l.items.borrow().len())
}

unsafe extern "C" fn list_get(list: RawValue, index: usize) -> RawValue {
    let v = std::mem::ManuallyDrop::new(Value::from_raw(list));
    v.as_list()
        .and_then(|l| l.items.borrow().get(index).cloned())
        .map_or(RawValue::NULL, Value::into_raw)
}

unsafe extern "C" fn list_push(list: RawValue, item: RawValue) {
    let item = Value::from_raw(item);
    let v = std::mem::ManuallyDrop::new(Value::from_raw(list));
    if let Some(l) = v.as_list() {
        l.items.borrow_mut().push(item);
    }
}

unsafe extern "C" fn call(
    c: *mut HostCtx,
    func: RawValue,
    args: *const RawValue,
    argc: usize,
    out: *mut RawValue,
) -> Status {
    let c = ctx(c);
    let func = std::mem::ManuallyDrop::new(Value::from_raw(func));
    let args: &[Value] = if argc == 0 { &[] } else { std::slice::from_raw_parts(args as *const Value, argc) };
    match c.rt.call_value(&func, args) {
        Ok(v) => {
            *out = v.into_raw();
            STATUS_OK
        }
        Err(e) => {
            c.pending = Some(e);
            STATUS_ERROR
        }
    }
}

unsafe extern "C" fn throw(c: *mut HostCtx, code: Str, args: *const RawValue, argc: usize) -> Status {
    let c = ctx(c);
    let args = if argc == 0 { &[][..] } else { std::slice::from_raw_parts(args, argc) };
    // A code of Hana's catalog is the runtime's own error, worded as Hana words it.
    let hana = crate::catalog::CATALOG.binary_search_by(|e| e.0.cmp(code.as_str())).is_ok();
    c.pending = Some(RuntimeError {
        module: if hana { None } else { Some(c.module) },
        code: code.as_str().to_string(),
        args: args.iter().map(|&a| Value::from_borrowed(a)).collect(),
    });
    STATUS_ERROR
}

unsafe extern "C" fn throw_type(c: *mut HostCtx, index: usize, expected: u32) -> Status {
    ctx(c).pending = Some(type_error(index, expected));
    STATUS_ERROR
}

pub(crate) static HOST_API: HostApi = HostApi {
    abi_version: ABI_VERSION,
    size: std::mem::size_of::<HostApi>() as u32,
    retain,
    release,
    str_view,
    str_new,
    list_new,
    list_len,
    list_get,
    list_push,
    call,
    throw,
    throw_type,
};
