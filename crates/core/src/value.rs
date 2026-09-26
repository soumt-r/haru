//! The runtime's value: an ABI [`RawValue`] that owns a reference.
//!
//! Heap values are `Rc` objects whose pointer sits in `payload`; `Clone` and
//! `Drop` adjust the count by tag. Keeping the ABI layout means a slice of
//! values can be handed to a native function as it is.

use std::cell::RefCell;
use std::collections::HashMap;

/// The maps the running program uses (keys are symbols and values; no
/// hashing attacks to fear, so a fast hasher).
pub type Map<K, V> = HashMap<K, V, rustc_hash::FxBuildHasher>;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::mem::ManuallyDrop;
use std::rc::Rc;

use haru_abi::{tag, RawValue};

/// Marks a variable slot whose variable does not exist (yet). Never seen by
/// programs or modules.
pub const UNDEF: u32 = u32::MAX;

/// A class used as a value (`[점]`, `'우리'`): the payload is the class id.
/// Not reference counted: classes live as long as the program.
pub const CLASS: u32 = 20;

pub struct StrObj {
    pub text: String,
}

pub struct ListObj {
    pub items: RefCell<Vec<Value>>,
}

/// A dictionary. Keys compare the way Hana's (Go map) keys do: numbers,
/// strings and booleans by value, lists and objects by identity.
pub struct DictObj {
    pub map: RefCell<Map<Key, Value>>,
}

/// An instance: its class and its properties (created by assignment, as in
/// Hana, so not a fixed layout). Keys are symbols (`crate::symbol`).
pub struct ObjObj {
    pub class: u32,
    pub props: RefCell<Props>,
}

/// An object's properties in the order they were first set: a class's
/// field initializers run in declaration order, so a field sits at the same
/// place in every object of the class. Objects have few properties, so a
/// search through the names beats hashing them (and needs no table). The
/// layout is fixed (`Stack`, `Prop`): compiled code reads and writes a
/// property at the place it saw it last (an inline cache).
#[derive(Default)]
#[repr(transparent)]
pub struct Props(pub(crate) crate::stack::Stack<Prop>);

#[repr(C)]
pub struct Prop {
    pub name: u32,
    pub value: Value,
}

impl Props {
    pub fn with_capacity(n: usize) -> Props {
        Props(crate::stack::Stack::with_capacity(n))
    }

    #[inline]
    pub fn get(&self, name: &u32) -> Option<&Value> {
        self.0.iter().find(|p| p.name == *name).map(|p| &p.value)
    }

    /// Where a property is (for an inline cache).
    #[inline]
    pub fn position(&self, name: u32) -> Option<usize> {
        self.0.iter().position(|p| p.name == name)
    }

    #[inline]
    pub fn contains_key(&self, name: &u32) -> bool {
        self.0.iter().any(|p| p.name == *name)
    }

    /// Sets a property (a new one goes last); the old value if there was one.
    #[inline]
    pub fn insert(&mut self, name: u32, v: Value) -> Option<Value> {
        match self.0.iter_mut().find(|p| p.name == name) {
            Some(p) => Some(std::mem::replace(&mut p.value, v)),
            None => {
                self.0.push(Prop { name, value: v });
                None
            }
        }
    }

    pub fn values(&self) -> impl Iterator<Item = &Value> {
        self.0.iter().map(|p| &p.value)
    }

    pub fn drain(&mut self) -> impl Iterator<Item = (u32, Value)> {
        self.0.take_all().into_iter().map(|p| (p.name, p.value))
    }
}

/// A native object (a resource): the object a module gave and its kind,
/// whose `drop` frees it when the last reference goes.
pub struct ResObj {
    pub kind: *const haru_abi::ResourceDesc,
    pub ptr: *mut std::ffi::c_void,
}

impl Drop for ResObj {
    fn drop(&mut self) {
        unsafe { ((*self.kind).drop)(self.ptr) }
    }
}

impl ResObj {
    /// Its kind's name in a language (its id without one).
    pub fn name(&self, lang: &str) -> &str {
        let k = unsafe { &*self.kind };
        let names = if k.names_len == 0 { &[][..] } else { unsafe { std::slice::from_raw_parts(k.names, k.names_len) } };
        names
            .iter()
            .find(|n| unsafe { n.lang.as_str() } == lang)
            .map_or_else(|| unsafe { k.id.as_str() }, |n| unsafe { n.name.as_str() })
    }
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
        let r = Rc::new(ListObj { items: RefCell::new(items) });
        crate::gc::track_list(&r);
        Value::heap(tag::LIST, r)
    }

    pub fn dict(map: Map<Key, Value>) -> Value {
        let r = Rc::new(DictObj { map: RefCell::new(map) });
        crate::gc::track_dict(&r);
        Value::heap(tag::DICT, r)
    }

    pub fn func(f: FuncObj) -> Value {
        Value::heap(tag::FUNC, Rc::new(f))
    }

    pub fn object(class: u32) -> Value {
        Value::object_with(class, 0)
    }

    /// A new object with room for `fields` properties.
    pub fn object_with(class: u32, fields: usize) -> Value {
        let r = Rc::new(ObjObj { class, props: RefCell::new(Props::with_capacity(fields)) });
        crate::gc::track_object(&r);
        Value::heap(tag::OBJECT, r)
    }

    pub fn resource(kind: *const haru_abi::ResourceDesc, ptr: *mut std::ffi::c_void) -> Value {
        Value::heap(tag::RESOURCE, Rc::new(ResObj { kind, ptr }))
    }

    pub fn as_resource(&self) -> Option<&ResObj> {
        (self.0.tag == tag::RESOURCE).then(|| unsafe { &*(self.0.payload as *const ResObj) })
    }

    pub fn class(id: u32) -> Value {
        Value(RawValue { tag: CLASS, pad: 0, payload: id as u64 })
    }

    pub fn as_class(&self) -> Option<u32> {
        (self.0.tag == CLASS).then_some(self.0.payload as u32)
    }

    pub fn as_object(&self) -> Option<&ObjObj> {
        (self.0.tag == tag::OBJECT).then(|| unsafe { &*(self.0.payload as *const ObjObj) })
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
        tag::OBJECT => Rc::increment_strong_count(raw.payload as *const ObjObj),
        tag::RESOURCE => Rc::increment_strong_count(raw.payload as *const ResObj),
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
        tag::OBJECT => Rc::decrement_strong_count(raw.payload as *const ObjObj),
        tag::RESOURCE => Rc::decrement_strong_count(raw.payload as *const ResObj),
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
