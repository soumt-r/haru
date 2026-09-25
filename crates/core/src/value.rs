//! The runtime's value: an ABI [`RawValue`] that owns a reference.
//!
//! Heap values are `Rc` objects whose pointer sits in `payload`; `Clone` and
//! `Drop` adjust the count by tag. Keeping the ABI layout means a slice of
//! values can be handed to a native function as it is.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::mem::ManuallyDrop;
use std::rc::Rc;

use haru_abi::{tag, RawValue};

/// Marks a variable slot whose variable does not exist (yet). Never seen by
/// programs or modules.
pub const UNDEF: u32 = u32::MAX;

pub struct StrObj {
    pub text: String,
}

pub struct ListObj {
    pub items: RefCell<Vec<Value>>,
}

/// A dictionary. Keys compare the way Hana's (Go map) keys do: numbers,
/// strings and booleans by value, lists and objects by identity.
pub struct DictObj {
    pub map: RefCell<HashMap<Key, Value>>,
}

pub enum FuncObj {
    /// A function of a native module.
    Native { module: usize, func: usize },
    /// A function declared in the program (index of its compiled body).
    User(u32),
    /// A built-in like `<문자로>`.
    Builtin(u8),
}

#[repr(transparent)]
pub struct Value(RawValue);

impl Value {
    pub const NULL: Value = Value(RawValue::NULL);
    pub const UNDEF: Value = Value(RawValue { tag: UNDEF, pad: 0, payload: 0 });

    #[inline]
    pub fn num(n: f64) -> Value {
        Value(RawValue::num(n))
    }

    #[inline]
    pub fn bool(b: bool) -> Value {
        Value(RawValue::bool(b))
    }

    pub fn str(s: &str) -> Value {
        Value::string(s.to_string())
    }

    pub fn string(text: String) -> Value {
        Value::heap(tag::STR, Rc::new(StrObj { text }))
    }

    pub fn list(items: Vec<Value>) -> Value {
        Value::heap(tag::LIST, Rc::new(ListObj { items: RefCell::new(items) }))
    }

    pub fn dict(map: HashMap<Key, Value>) -> Value {
        Value::heap(tag::DICT, Rc::new(DictObj { map: RefCell::new(map) }))
    }

    pub fn func(f: FuncObj) -> Value {
        Value::heap(tag::FUNC, Rc::new(f))
    }

    fn heap<T>(tag: u32, obj: Rc<T>) -> Value {
        Value(RawValue { tag, pad: 0, payload: Rc::into_raw(obj) as u64 })
    }

    #[inline]
    pub fn raw(&self) -> RawValue {
        self.0
    }

    #[inline]
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

    #[inline]
    pub fn is_undef(&self) -> bool {
        self.0.tag == UNDEF
    }

    #[inline]
    pub fn is_null(&self) -> bool {
        self.0.tag == tag::NULL
    }

    #[inline]
    pub fn as_num(&self) -> Option<f64> {
        self.0.as_num()
    }

    #[inline]
    pub fn as_bool(&self) -> Option<bool> {
        self.0.as_bool()
    }

    pub fn as_str(&self) -> Option<&str> {
        (self.0.tag == tag::STR).then(|| unsafe { (*(self.0.payload as *const StrObj)).text.as_str() })
    }

    pub fn as_list(&self) -> Option<&ListObj> {
        (self.0.tag == tag::LIST).then(|| unsafe { &*(self.0.payload as *const ListObj) })
    }

    pub fn as_dict(&self) -> Option<&DictObj> {
        (self.0.tag == tag::DICT).then(|| unsafe { &*(self.0.payload as *const DictObj) })
    }

    pub fn as_func(&self) -> Option<&FuncObj> {
        (self.0.tag == tag::FUNC).then(|| unsafe { &*(self.0.payload as *const FuncObj) })
    }

    /// Whether both are the same heap object.
    #[inline]
    pub fn same_object(&self, other: &Value) -> bool {
        self.0.tag == other.0.tag && self.0.payload == other.0.payload
    }

    /// Appends to this string in place when nothing else holds it; false
    /// (and nothing done) otherwise.
    pub fn append_in_place(&mut self, s: &str) -> bool {
        if self.0.tag != tag::STR {
            return false;
        }
        let mut rc = ManuallyDrop::new(unsafe { Rc::from_raw(self.0.payload as *const StrObj) });
        match Rc::get_mut(&mut rc) {
            Some(obj) => {
                obj.text.push_str(s);
                true
            }
            None => false,
        }
    }

    /// Hana's `==`: numbers, strings, booleans and null by value; lists,
    /// dictionaries, objects and functions by identity.
    pub fn go_eq(&self, other: &Value) -> bool {
        match (self.0.tag, other.0.tag) {
            (tag::NUM, tag::NUM) => self.as_num() == other.as_num(),
            (tag::STR, tag::STR) => self.as_str() == other.as_str(),
            (a, b) => a == b && self.0.payload == other.0.payload,
        }
    }
}

/// # Safety
/// `raw` must be a live value this runtime created.
pub(crate) unsafe fn retain(raw: RawValue) {
    match raw.tag {
        tag::STR => Rc::increment_strong_count(raw.payload as *const StrObj),
        tag::LIST => Rc::increment_strong_count(raw.payload as *const ListObj),
        tag::DICT => Rc::increment_strong_count(raw.payload as *const DictObj),
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
        tag::DICT => Rc::decrement_strong_count(raw.payload as *const DictObj),
        tag::FUNC => Rc::decrement_strong_count(raw.payload as *const FuncObj),
        _ => {}
    }
}

/// Heap tags (STR..=RESOURCE); unlike `tag::is_heap`, not UNDEF.
#[inline(always)]
fn counted(t: u32) -> bool {
    t.wrapping_sub(tag::STR) <= tag::RESOURCE - tag::STR
}

impl Clone for Value {
    #[inline]
    fn clone(&self) -> Value {
        if counted(self.0.tag) {
            unsafe { retain(self.0) }
        }
        Value(self.0)
    }
}

impl Drop for Value {
    #[inline]
    fn drop(&mut self) {
        if counted(self.0.tag) {
            unsafe { release(self.0) }
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Value) -> bool {
        self.go_eq(other)
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.tag {
            tag::STR => write!(f, "{:?}", self.as_str().unwrap()),
            UNDEF => f.write_str("<undef>"),
            _ => f.write_str(&crate::format::display(self, &crate::lang::HARI)),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&crate::format::display(self, &crate::lang::HARI))
    }
}

/// A dictionary key: a value compared like a Go map key.
#[derive(Clone)]
pub struct Key(pub Value);

impl Key {
    /// Dictionaries cannot be keys (Go cannot hash maps).
    pub fn new(v: Value) -> Option<Key> {
        (v.tag() != tag::DICT && !v.is_undef()).then_some(Key(v))
    }
}

impl PartialEq for Key {
    fn eq(&self, other: &Key) -> bool {
        self.0.go_eq(&other.0)
    }
}

impl Eq for Key {}

impl Hash for Key {
    fn hash<H: Hasher>(&self, h: &mut H) {
        let v = &self.0;
        v.tag().hash(h);
        match v.tag() {
            // -0 and 0 are the same key
            tag::NUM => (v.as_num().unwrap() + 0.0).to_bits().hash(h),
            tag::STR => v.as_str().unwrap().hash(h),
            _ => v.raw().payload.hash(h),
        }
    }
}

const _: () = assert!(std::mem::size_of::<Value>() == 16);
