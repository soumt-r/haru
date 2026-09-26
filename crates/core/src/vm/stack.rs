//! A growable stack whose layout is fixed (pointer, length, capacity), so
//! that compiled code (the JIT) can push and pop on it directly. It is a
//! `Vec` otherwise: growing goes through one.

use std::mem::ManuallyDrop;
use std::ops::{Deref, DerefMut};

#[repr(C)]
pub(super) struct Stack<T> {
    pub(super) ptr: *mut T,
    pub(super) len: usize,
    pub(super) cap: usize,
}

impl<T> Stack<T> {
    pub(super) fn with_capacity(n: usize) -> Stack<T> {
        let mut v = ManuallyDrop::new(Vec::with_capacity(n));
        Stack { ptr: v.as_mut_ptr(), len: v.len(), cap: v.capacity() }
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
        let vec = ManuallyDrop::new(unsafe { Vec::from_raw_parts(self.ptr, self.len, self.cap) });
        let mut back = Back { stack: self, vec };
        f(&mut back.vec)
    }

    #[inline]
    pub(super) fn push(&mut self, v: T) {
        if self.len < self.cap {
            unsafe { self.ptr.add(self.len).write(v) };
            self.len += 1;
        } else {
            self.edit(|s| s.push(v));
        }
    }

    #[inline]
    pub(super) fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        Some(unsafe { self.ptr.add(self.len).read() })
    }

    #[inline]
    pub(super) fn truncate(&mut self, len: usize) {
        self.edit(|s| s.truncate(len));
    }

    pub(super) fn extend(&mut self, items: impl IntoIterator<Item = T>) {
        self.edit(|s| s.extend(items));
    }
}

impl<T: Clone> Stack<T> {
    #[inline]
    pub(super) fn resize(&mut self, len: usize, v: T) {
        self.edit(|s| s.resize(len, v));
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

impl<T> Drop for Stack<T> {
    fn drop(&mut self) {
        unsafe { drop(Vec::from_raw_parts(self.ptr, self.len, self.cap)) };
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
}
