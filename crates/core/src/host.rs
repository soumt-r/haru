//! The [`HostApi`] this runtime gives to modules.

use haru_abi::{HostApi, HostCtx, RawValue, Status, Str, ABI_VERSION, STATUS_ERROR, STATUS_OK};

use crate::error::RuntimeError;
use crate::modules::{type_error, Caller, FnRef, Runtime};
use crate::value::FuncObj;
use crate::value::{self, Key, Value};

/// What a `*mut HostCtx` points at during a native call.
pub(crate) struct CallCtx<'a> {
    pub rt: &'a Runtime,
    pub module: usize,
    pub pending: Option<RuntimeError>,
    /// Runs the program's functions a native function calls back (the VM).
    pub caller: Option<*mut dyn Caller>,
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
    let result = match (func.as_func(), c.caller) {
        (Some(&FuncObj::Native { module, func }), caller) => c.rt.call_with(FnRef { module, func }, args, caller),
        (_, Some(caller)) => (*caller).call(&func, args),
        _ => c.rt.call_value(&func, args),
    };
    match result {
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

unsafe extern "C" fn list_set(list: RawValue, index: usize, item: RawValue) -> bool {
    let item = Value::from_raw(item);
    let v = std::mem::ManuallyDrop::new(Value::from_raw(list));
    let Some(l) = v.as_list() else { return false };
    let mut items = l.items.borrow_mut();
    match items.get_mut(index) {
        Some(slot) => {
            *slot = item;
            true
        }
        None => false,
    }
}

unsafe extern "C" fn dict_new() -> RawValue {
    Value::dict(Default::default()).into_raw()
}

unsafe extern "C" fn dict_len(dict: RawValue) -> usize {
    let v = std::mem::ManuallyDrop::new(Value::from_raw(dict));
    v.as_dict().map_or(0, |d| d.map.borrow().len())
}

unsafe extern "C" fn dict_get(dict: RawValue, key: RawValue, out: *mut RawValue) -> bool {
    let v = std::mem::ManuallyDrop::new(Value::from_raw(dict));
    let key = std::mem::ManuallyDrop::new(Key(Value::from_raw(key)));
    match v.as_dict().and_then(|d| d.map.borrow().get(&key).cloned()) {
        Some(found) => {
            *out = found.into_raw();
            true
        }
        None => false,
    }
}

unsafe extern "C" fn dict_set(dict: RawValue, key: RawValue, value: RawValue) {
    let (key, value) = (Key(Value::from_raw(key)), Value::from_raw(value));
    let v = std::mem::ManuallyDrop::new(Value::from_raw(dict));
    if let Some(d) = v.as_dict() {
        d.map.borrow_mut().insert(key, value);
    }
}

unsafe extern "C" fn dict_keys(dict: RawValue) -> RawValue {
    let v = std::mem::ManuallyDrop::new(Value::from_raw(dict));
    let keys = v.as_dict().map_or(Vec::new(), |d| d.map.borrow().keys().map(|k| k.0.clone()).collect());
    Value::list(keys).into_raw()
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
    list_set,
    dict_new,
    dict_len,
    dict_get,
    dict_set,
    dict_keys,
};
