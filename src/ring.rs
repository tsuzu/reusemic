//! Lock-free single-producer / single-consumer ring of f32 samples.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct Ring {
    buf: Box<[UnsafeCell<f32>]>,
    /// Total samples ever written (producer-owned).
    head: AtomicUsize,
    /// Total samples ever read (consumer-owned).
    tail: AtomicUsize,
}

// Safety: one producer writes only slots in [head, tail + cap), one consumer reads only slots in
// [tail, head); the atomics order those accesses.
unsafe impl Sync for Ring {}
unsafe impl Send for Ring {}

impl Ring {
    pub fn new(capacity: usize) -> Self {
        Self {
            buf: (0..capacity).map(|_| UnsafeCell::new(0.0)).collect(),
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    pub fn len(&self) -> usize {
        self.head
            .load(Ordering::Acquire)
            .wrapping_sub(self.tail.load(Ordering::Acquire))
    }

    /// Producer: appends as much of `data` as fits; returns how many were written.
    pub fn push(&self, data: &[f32]) -> usize {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        let free = self.capacity() - head.wrapping_sub(tail);
        let n = data.len().min(free);
        let cap = self.capacity();
        for (i, &v) in data[..n].iter().enumerate() {
            unsafe { *self.buf[(head + i) % cap].get() = v };
        }
        self.head.store(head.wrapping_add(n), Ordering::Release);
        n
    }

    /// Consumer: fills the front of `out` with up to `out.len()` samples; returns how many.
    pub fn pop(&self, out: &mut [f32]) -> usize {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        let n = out.len().min(head.wrapping_sub(tail));
        let cap = self.capacity();
        for (i, o) in out[..n].iter_mut().enumerate() {
            *o = unsafe { *self.buf[(tail + i) % cap].get() };
        }
        self.tail.store(tail.wrapping_add(n), Ordering::Release);
        n
    }

    /// Consumer: drops up to `n` of the oldest samples.
    pub fn skip(&self, n: usize) {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        let n = n.min(head.wrapping_sub(tail));
        self.tail.store(tail.wrapping_add(n), Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_and_bounds() {
        let r = Ring::new(4);
        assert_eq!(r.push(&[1.0, 2.0, 3.0]), 3);
        let mut o = [0.0; 2];
        assert_eq!(r.pop(&mut o), 2);
        assert_eq!(o, [1.0, 2.0]);
        assert_eq!(r.push(&[4.0, 5.0, 6.0, 7.0]), 3);
        assert_eq!(r.len(), 4);
        let mut o = [0.0; 8];
        assert_eq!(r.pop(&mut o), 4);
        assert_eq!(&o[..4], &[3.0, 4.0, 5.0, 6.0]);
    }
}
