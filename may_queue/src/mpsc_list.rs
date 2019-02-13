use std::cell::UnsafeCell;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::thread;

use crossbeam::utils::CachePadded;

struct Node<T> {
    next: AtomicPtr<Node<T>>,
    value: Option<T>,
}

impl<T> Node<T> {
    unsafe fn new(v: Option<T>) -> *mut Node<T> {
        Box::into_raw(Box::new(Node {
            next: AtomicPtr::new(ptr::null_mut()),
            value: v,
        }))
    }
}

/// The multi-producer single-consumer structure. This is not cloneable, but it
/// may be safely shared so long as it is guaranteed that there is only one
/// popper at a time (many pushers are allowed).
pub struct Queue<T> {
    head: CachePadded<AtomicPtr<Node<T>>>,
    tail: UnsafeCell<*mut Node<T>>,
}

unsafe impl<T> Send for Queue<T> {}
unsafe impl<T> Sync for Queue<T> {}

impl<T> Queue<T> {
    /// Creates a new queue that is safe to share among multiple producers and
    /// one consumer.
    pub fn new() -> Queue<T> {
        let stub = unsafe { Node::new(None) };
        Queue {
            head: AtomicPtr::new(stub).into(),
            tail: UnsafeCell::new(stub),
        }
    }

    pub fn push(&self, t: T) {
        unsafe {
            let node = Node::new(Some(t));
            let prev = self.head.swap(node, Ordering::AcqRel);
            (*prev).next.store(node, Ordering::Release);
        }
    }

    /// if the queue is empty
    #[inline]
    pub fn is_empty(&self) -> bool {
        let tail = unsafe { *self.tail.get() };
        // the list is empty
        self.head.load(Ordering::Acquire) == tail
    }

    /// Pops some data from this queue.
    pub fn pop(&self) -> Option<T> {
        unsafe {
            let tail = *self.tail.get();

            // the list is empty
            if self.head.load(Ordering::Acquire) == tail {
                return None;
            }

            // spin until tail next become non-null
            let mut next;
            let mut i = 0;
            loop {
                next = (*tail).next.load(Ordering::Acquire);
                if !next.is_null() {
                    break;
                }
                i += 1;
                if i > 100 {
                    thread::yield_now();
                    i = 0;
                }
            }

            assert!((*tail).value.is_none());
            assert!((*next).value.is_some());
            // we tack the next value, this is why use option to host the value
            let ret = (*next).value.take().unwrap();
            let _: Box<Node<T>> = Box::from_raw(tail);

            // move the tail to next
            *self.tail.get() = next;

            Some(ret)
        }
    }
}

impl<T> Default for Queue<T> {
    fn default() -> Self {
        Queue::new()
    }
}

impl<T> Drop for Queue<T> {
    fn drop(&mut self) {
        while let Some(_) = self.pop() {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_queue() {
        let q: Queue<usize> = Queue::new();
        assert_eq!(q.pop(), None);
        q.push(1);
        q.push(2);
        assert_eq!(q.pop(), Some(1));
        assert_eq!(q.pop(), Some(2));
        assert_eq!(q.is_empty(), true);
    }

    #[test]
    fn test() {
        let nthreads = 8;
        let nmsgs = 1000;
        let q = Queue::new();
        match q.pop() {
            None => {}
            Some(..) => panic!(),
        }
        let (tx, rx) = channel();
        let q = Arc::new(q);

        for _ in 0..nthreads {
            let tx = tx.clone();
            let q = q.clone();
            thread::spawn(move || {
                for i in 0..nmsgs {
                    q.push(i);
                }
                tx.send(()).unwrap();
            });
        }

        let mut i = 0;
        while i < nthreads * nmsgs {
            match q.pop() {
                None => {}
                Some(_) => i += 1,
            }
        }
        drop(tx);
        for _ in 0..nthreads {
            rx.recv().unwrap();
        }
    }
}
