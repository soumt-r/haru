//! A growable stack whose layout is fixed (pointer, length, capacity), so
//! that compiled code (the JIT) can push and pop on it directly. It is a
//! `Vec` otherwise: growing goes through one.
//!
//! Taking the first item (a queue, `'목록' 앞에서 꺼내자`) moves the pointer
//! instead of the rest: the allocation then starts `off` slots before `ptr`,
//! and `cap` counts only the room from `ptr` on, so compiled code that sees
//! (ptr, len, cap) needs nothing more. Putting an item in front uses that
//! room again, and anything that needs a `Vec` moves the items back first.

use std::mem::ManuallyDrop;
use std::ops::{Deref, DerefMut};

#[repr(C)]
pub struct Stack<T> {
    pub(crate) ptr: *mut T,
    pub(crate) len: usize,
    pub(crate) cap: usize,
    /// Free slots before `ptr` (items taken from the front).
    off: usize,
}

impl<T> Stack<T> {
    pub(crate) fn with_capacity(n: usize) -> Stack<T> {
        let mut v = ManuallyDrop::new(Vec::with_capacity(n));
        Stack { ptr: v.as_mut_ptr(), len: v.len(), cap: v.capacity(), off: 0 }
    }

    /// Moves the items back to the start of the allocation.
    fn compact(&mut self) {
        if self.off == 0 {
            return;
        }
        unsafe {
            let base = self.ptr.sub(self.off);
            std::ptr::copy(self.ptr, base, self.len);
            self.ptr = base;
        }
        self.cap += self.off;
        self.off = 0;
    }

    /// Runs `f` on the stack as a `Vec` (and takes back what it became, even
    /// when `f` panics).
    #[inline]
    fn edit<R>(&mut self, f: impl FnOnce(&mut Vec<T>) -> R) -> R {
        struct Back<'a, T> {
            stack: &'a mut Stack<T>,
            vec: ManuallyDrop<Vec<T>>,
        }
        impl<T> Drop for Back<'_, T> {
            fn drop(&mut self) {
                self.stack.ptr = self.vec.as_mut_ptr();
                self.stack.len = self.vec.len();
                self.stack.cap = self.vec.capacity();
            }
        }
        self.compact();
        let vec = ManuallyDrop::new(unsafe { Vec::from_raw_parts(self.ptr, self.len, self.cap) });
        let mut back = Back { stack: self, vec };
        f(&mut back.vec)
    }

    #[inline]
    pub fn push(&mut self, v: T) {
        if self.len < self.cap {
            unsafe { self.ptr.add(self.len).write(v) };
            self.len += 1;
        } else if self.off >= self.len.max(1) {
            // At least as many items were taken from the front as are left:
            // moving these back pays for itself.
            self.compact();
            unsafe { self.ptr.add(self.len).write(v) };
            self.len += 1;
        } else {
            // Grow by doubling even when there is room at the front, or a
            // queue that takes one and adds one would move everything each time.
            self.edit(|s| {
                s.reserve(s.len().max(1));
                s.push(v)
            });
        }
    }

    #[inline]
    pub fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        Some(unsafe { self.ptr.add(self.len).read() })
    }

    #[inline]
    pub fn truncate(&mut self, len: usize) {
        if len >= self.len {
            return;
        }
        let old = self.len;
        // Shorter first: a drop that panics leaks the rest instead of
        // dropping it twice.
        self.len = len;
        unsafe { std::ptr::drop_in_place(std::slice::from_raw_parts_mut(self.ptr.add(len), old - len)) };
    }

    /// Drops the last item where it is.
    #[inline]
    pub(crate) fn drop_last(&mut self) {
        if self.len > 0 {
            self.len -= 1;
            unsafe { std::ptr::drop_in_place(self.ptr.add(self.len)) };
        }
    }

    pub(crate) fn extend(&mut self, items: impl IntoIterator<Item = T>) {
        self.edit(|s| s.extend(items));
    }

    /// Takes everything out.
    pub(crate) fn take_all(&mut self) -> Vec<T> {
        self.edit(std::mem::take)
    }

    pub fn from_vec(v: Vec<T>) -> Stack<T> {
        let mut v = ManuallyDrop::new(v);
        Stack { ptr: v.as_mut_ptr(), len: v.len(), cap: v.capacity(), off: 0 }
    }

    pub fn insert(&mut self, i: usize, v: T) {
        if i == 0 && self.off > 0 {
            unsafe {
                self.ptr = self.ptr.sub(1);
                self.ptr.write(v);
            }
            self.off -= 1;
            self.cap += 1;
            self.len += 1;
            return;
        }
        self.edit(|s| s.insert(i, v));
    }

    pub fn remove(&mut self, i: usize) -> T {
        if i == 0 && self.len > 0 {
            let v = unsafe { self.ptr.read() };
            self.ptr = unsafe { self.ptr.add(1) };
            self.len -= 1;
            self.cap -= 1;
            self.off += 1;
            return v;
        }
        self.edit(|s| s.remove(i))
    }

    pub fn clear(&mut self) {
        self.edit(Vec::clear);
    }
}

impl<T: Clone> Stack<T> {
    #[inline]
    pub(crate) fn resize(&mut self, len: usize, v: T) {
        if len <= self.len {
            self.truncate(len);
        } else if len <= self.cap {
            // Room already: write the new items in place.
            for i in self.len..len {
                unsafe { self.ptr.add(i).write(v.clone()) };
                self.len = i + 1;
            }
        } else {
            self.edit(|s| s.resize(len, v));
        }
    }
}

impl<T> Deref for Stack<T> {
    type Target = [T];
    #[inline]
    fn deref(&self) -> &[T] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl<T> DerefMut for Stack<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut [T] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl<T> Default for Stack<T> {
    fn default() -> Stack<T> {
        Stack::with_capacity(0)
    }
}

impl<T> Drop for Stack<T> {
    fn drop(&mut self) {
        unsafe {
            // A panicking item drop leaks the allocation rather than freeing it twice.
            std::ptr::drop_in_place(std::slice::from_raw_parts_mut(self.ptr, self.len));
            drop(Vec::from_raw_parts(self.ptr.sub(self.off), 0, self.cap + self.off));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Stack;

    #[test]
    fn behaves_as_a_vec() {
        let mut s: Stack<String> = Stack::with_capacity(1);
        s.push("a".into());
        s.push("b".into());
        s.extend(["c".to_string(), "d".to_string()]);
        assert_eq!(&s[..], ["a", "b", "c", "d"]);
        assert_eq!(s.pop().as_deref(), Some("d"));
        s.truncate(1);
        s.resize(3, "z".into());
        assert_eq!(&s[..], ["a", "z", "z"]);
        s[1] = "y".into();
        assert_eq!(s.last().map(String::as_str), Some("z"));
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn takes_from_the_front_without_moving_the_rest() {
        let mut s: Stack<String> = Stack::with_capacity(4);
        for x in ["a", "b", "c", "d"] {
            s.push(x.into());
        }
        let ptr = s.ptr;
        assert_eq!(s.remove(0), "a");
        assert_eq!(s.remove(0), "b");
        assert_eq!(unsafe { s.ptr.offset_from(ptr) }, 2);
        assert_eq!((s.len, s.cap, s.off), (2, 2, 2));
        // The room in front is used again.
        s.insert(0, "z".into());
        assert_eq!(&s[..], ["z", "c", "d"]);
        // Full at the back, as much room in front as items: move back, no new allocation.
        s.remove(0);
        s.remove(0);
        s.push("e".into());
        assert_eq!(&s[..], ["d", "e"]);
        assert_eq!(s.ptr, ptr);
        // Anything else goes through a Vec, after moving back.
        s.insert(1, "m".into());
        s.extend(["x".to_string()]);
        assert_eq!(&s[..], ["d", "m", "e", "x"]);
        assert_eq!(s.take_all(), ["d", "m", "e", "x"]);
    }

    #[test]
    fn a_steady_queue_grows_by_doubling() {
        let mut s: Stack<usize> = Stack::with_capacity(0);
        for i in 0..1000 {
            s.push(i);
        }
        for i in 1000..200_000 {
            assert_eq!(s.remove(0), i - 1000);
            s.push(i);
        }
        assert_eq!(s.len(), 1000);
        assert!(s.cap + s.off < 8 * 1000, "{} + {}", s.cap, s.off);
    }

    #[test]
    fn drops_what_is_left() {
        use std::rc::Rc;
        let one = Rc::new(());
        let mut s = Stack::with_capacity(0);
        for _ in 0..5 {
            s.push(one.clone());
        }
        s.remove(0);
        s.remove(0);
        assert_eq!(Rc::strong_count(&one), 4);
        drop(s);
        assert_eq!(Rc::strong_count(&one), 1);
    }
}
