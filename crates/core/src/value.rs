//! The runtime's value: an ABI [`RawValue`] that owns a reference.
//!
//! Heap values are `Rc` objects whose pointer sits in `payload`; `Clone` and
//! `Drop` adjust the count by tag. Keeping the ABI layout means a slice of
//! values can be handed to a native function as it is.

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use haru_abi::{tag, RawValue};

pub struct StrObj {
    pub text: Box<str>,
}

pub struct ListObj {
    pub items: RefCell<Vec<Value>>,
}

/// A function value. Only native functions exist until the VM lands.
pub struct FuncObj {
    pub module: usize,
    pub func: usize,
}

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
        Value::heap(tag::STR, Rc::new(StrObj { text: s.into() }))
    }

    pub fn list(items: Vec<Value>) -> Value {
        Value::heap(tag::LIST, Rc::new(ListObj { items: RefCell::new(items) }))
    }

    pub fn func(f: FuncObj) -> Value {
        Value::heap(tag::FUNC, Rc::new(f))
    }

    fn heap<T>(tag: u32, obj: Rc<T>) -> Value {
        Value(RawValue { tag, pad: 0, payload: Rc::into_raw(obj) as u64 })
    }

    pub fn raw(&self) -> RawValue {
        self.0
    }

    pub fn tag(&self) -> u32 {
        self.0.tag
    }

    /// Takes over the reference a raw value carries (+1 from a native).
    ///
    /// # Safety
    /// `raw` must be a value this runtime created, with a reference to give.
    pub unsafe fn from_raw(raw: RawValue) -> Value {
        Value(raw)
    }

    /// Takes a new reference to a borrowed raw value.
    ///
    /// # Safety
    /// `raw` must be a live value this runtime created.
    pub unsafe fn from_borrowed(raw: RawValue) -> Value {
        retain(raw);
        Value(raw)
    }

    pub fn into_raw(self) -> RawValue {
        let raw = self.0;
        std::mem::forget(self);
        raw
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

    pub fn as_str(&self) -> Option<&str> {
        (self.0.tag == tag::STR).then(|| unsafe { &*(*(self.0.payload as *const StrObj)).text })
    }

    pub fn as_list(&self) -> Option<&ListObj> {
        (self.0.tag == tag::LIST).then(|| unsafe { &*(self.0.payload as *const ListObj) })
    }

    pub fn as_func(&self) -> Option<&FuncObj> {
        (self.0.tag == tag::FUNC).then(|| unsafe { &*(self.0.payload as *const FuncObj) })
    }
}

/// # Safety
/// `raw` must be a live value this runtime created.
pub(crate) unsafe fn retain(raw: RawValue) {
    match raw.tag {
        tag::STR => Rc::increment_strong_count(raw.payload as *const StrObj),
        tag::LIST => Rc::increment_strong_count(raw.payload as *const ListObj),
        tag::FUNC => Rc::increment_strong_count(raw.payload as *const FuncObj),
        _ => {}
    }
}

/// # Safety
/// `raw` must carry a reference this call may give up.
pub(crate) unsafe fn release(raw: RawValue) {
    match raw.tag {
        tag::STR => Rc::decrement_strong_count(raw.payload as *const StrObj),
        tag::LIST => Rc::decrement_strong_count(raw.payload as *const ListObj),
        tag::FUNC => Rc::decrement_strong_count(raw.payload as *const FuncObj),
        _ => {}
    }
}

impl Clone for Value {
    fn clone(&self) -> Value {
        unsafe { Value::from_borrowed(self.0) }
    }
}

impl Drop for Value {
    fn drop(&mut self) {
        if tag::is_heap(self.0.tag) {
            unsafe { release(self.0) }
        }
    }
}

impl PartialEq for Value {
    /// Numbers, strings and bools by value; everything else by reference
    /// (lists are reference types, spec 1.2).
    fn eq(&self, other: &Value) -> bool {
        match (self.0.tag, other.0.tag) {
            (tag::NUM, tag::NUM) => self.as_num() == other.as_num(),
            (tag::STR, tag::STR) => self.as_str() == other.as_str(),
            (a, b) => a == b && self.0.payload == other.0.payload,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.tag {
            tag::NULL => f.write_str("비어있음"),
            tag::BOOL => f.write_str(if self.0.payload != 0 { "참" } else { "거짓" }),
            tag::NUM => write!(f, "{}", self.as_num().unwrap()),
            tag::STR => f.write_str(self.as_str().unwrap()),
            tag::LIST => {
                f.write_str("[")?;
                for (i, item) in self.as_list().unwrap().items.borrow().iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{item}")?;
                }
                f.write_str("]")
            }
            tag::FUNC => f.write_str("<함수>"),
            t => write!(f, "<값 {t}>"),
        }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.tag {
            tag::STR => write!(f, "{:?}", self.as_str().unwrap()),
            _ => write!(f, "{self}"),
        }
    }
}

const _: () = assert!(std::mem::size_of::<Value>() == 16);
