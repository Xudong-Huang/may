//! A mostly lock-free multi-producer, single consumer queue.
//!
//! This module contains an implementation of a concurrent MPSC queue. This
//! queue can be used to share data between threads, and is also used as the
//! building block of channels in rust.
//!
//! Note that the current implementation of this queue has a caveat of the `pop`
//! method, and see the method for more information about it. Due to this
//! caveat, this queue may not be appropriate for all use-cases.

// http://www.1024cores.net/home/lock-free-algorithms
//                         /queues/non-intrusive-mpsc-node-based-queue

use std::cell::UnsafeCell;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

use self::PopResult::*;
use crate::yield_now::yield_now;

/// A result of the `pop` function.
enum PopResult<T> {
    /// Some data has been popped
    Data(T),
    /// The queue is empty
    Empty,
    /// The queue is in an inconsistent state. Popping data should succeed, but
    /// some pushers have yet to make enough progress in order allow a pop to
    /// succeed. It is recommended that a pop() occur "in the near future" in
    /// order to see if the sender has made progress or not
    Inconsistent,
}

struct Node<T> {
    next: AtomicPtr<Node<T>>,
    value: Option<T>,
}

/// The multi-producer single-consumer structure. This is not cloneable, but it
/// may be safely shared so long as it is guaranteed that there is only one
/// popper at a time (many pushers are allowed).
pub struct Queue<T> {
    head: AtomicPtr<Node<T>>,
    tail: UnsafeCell<*mut Node<T>>,
}

unsafe impl<T: Send> Send for Queue<T> {}
unsafe impl<T: Send> Sync for Queue<T> {}

impl<T> Node<T> {
    unsafe fn new(v: Option<T>) -> *mut Node<T> {
        Box::into_raw(Box::new(Node {
            next: AtomicPtr::new(ptr::null_mut()),
            value: v,
        }))
    }
}

impl<T> Queue<T> {
    /// Creates a new queue that is safe to share among multiple producers and
    /// one consumer.
    pub fn new() -> Queue<T> {
        let stub = unsafe { Node::new(None) };
        Queue {
            head: AtomicPtr::new(stub),
            tail: UnsafeCell::new(stub),
        }
    }

    /// Pushes a new value onto this queue.
    pub fn push(&self, t: T) {
        unsafe {
            let n = Node::new(Some(t));
            let prev = self.head.swap(n, Ordering::AcqRel);
            (*prev).next.store(n, Ordering::Release);
        }
    }

    /// if the queue is empty
    #[allow(dead_code)]
    #[inline]
    pub fn is_empty(&self) -> bool {
        let tail = unsafe { *self.tail.get() };
        // the list is empty
        self.head.load(Ordering::Acquire) == tail
    }

    fn raw_pop(&self) -> PopResult<T> {
        unsafe {
            let tail = *self.tail.get();
            let next = (*tail).next.load(Ordering::Acquire);

            if !next.is_null() {
                *self.tail.get() = next;
                // value is not an atomic operation it may read out old shadow value
                // assert!((*tail).value.is_none());
                assert!((*next).value.is_some());
                let ret = (*next).value.take().unwrap();
                let _: Box<Node<T>> = Box::from_raw(tail);
                return Data(ret);
            }

            if self.head.load(Ordering::Acquire) == tail {
                Empty
            } else {
                Inconsistent
            }
        }
    }

    /// Pops some data from this queue.
    pub fn pop(&self) -> Option<T> {
        match self.raw_pop() {
            Empty => None,
            Data(ret) => Some(ret),
            Inconsistent => loop {
                yield_now();
                match self.raw_pop() {
                    Data(ret) => return Some(ret),
                    Empty => panic!("inconsistent => empty"),
                    Inconsistent => {}
                }
            },
        }
    }
}

impl<T> Drop for Queue<T> {
    fn drop(&mut self) {
        while let Some(_) = self.pop() {}
        // release the stub
        let _: Box<Node<T>> = unsafe { Box::from_raw(*self.tail.get()) };
    }
}
