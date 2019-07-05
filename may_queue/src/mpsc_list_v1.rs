use std::cell::UnsafeCell;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::thread;

use crossbeam::utils::CachePadded;

struct Node<T> {
    prev: *mut Node<T>,
    next: AtomicPtr<Node<T>>,
    value: Option<T>,
    refs: usize,
}
// linked bit is MSB, ref count is 2 for handle and list
const REF_INIT: usize = 0x1000_0002;
const REF_COUNT_MASK: usize = 0x0FFF_FFFF;

impl<T> Node<T> {
    unsafe fn new(v: Option<T>) -> *mut Node<T> {
        Box::into_raw(Box::new(Node {
            prev: ptr::null_mut(),
            next: AtomicPtr::new(ptr::null_mut()),
            value: v,
            refs: REF_INIT,
        }))
    }
}

pub struct Entry<T>(ptr::NonNull<Node<T>>);

unsafe impl<T: Sync> Sync for Entry<T> {}

impl<T> Entry<T> {
    // get the internal data mut ref
    // must make sure it's not popped by the consumer
    #[inline]
    pub unsafe fn with_mut_data<F>(&self, f: F)
    where
        F: FnOnce(&mut T),
    {
        let node = &mut *self.0.as_ptr();
        let data = node.value.as_mut().expect("Node value is None");
        f(data);
    }

    /// judge if the node is still linked in the list
    #[inline]
    pub fn is_link(&self) -> bool {
        let node = unsafe { &mut *self.0.as_ptr() };
        node.refs & !REF_COUNT_MASK != 0
    }

    #[inline]
    pub fn into_ptr(self) -> *mut Self {
        let ret = self.0.as_ptr() as *mut Self;
        ::std::mem::forget(self);
        ret
    }

    #[inline]
    pub unsafe fn from_ptr(ptr: *mut Self) -> Self {
        Entry(ptr::NonNull::new_unchecked(ptr as *mut Node<T>))
    }

    // remove the entry from it's list and return the contained value
    // it's only safe for the consumer that call pop()
    pub fn remove(mut self) -> Option<T> {
        unsafe {
            let node = self.0.as_mut();

            // when the link bit is cleared, next and prev is no longer valid
            if node.refs & !REF_COUNT_MASK == 0 {
                // already removed
                return None;
            }

            // this is a new tail just return
            if node.prev.is_null() {
                return None;
            }

            let next = node.next.load(Ordering::Acquire);
            let prev = &mut *node.prev;

            // here we must make sure the next is not equal to null
            // other thread may modify the next value if it's null
            // it's safe to remove the node that between tail and head
            // but not safe to remove the last node since it's volatile
            // when next is null, the remove takes no action
            // and expect pop() would eventually consume the data
            // this is mainly used in the timer list, so it's rarely
            // the next is not contention for that we have wait some time already
            // leave the last node not removed also persist the queue for a while
            // that prevent frequent queue create and destroy
            if !next.is_null() {
                // clear the link bit
                node.refs &= REF_COUNT_MASK;

                // this is not the last node, just unlink it
                (*next).prev = prev;
                prev.next.store(next, Ordering::Release);

                let ret = node.value.take();

                // since self is not dropped, below is always false
                node.refs -= 1;
                if node.refs == 0 {
                    // release the node only when the ref count becomes 0
                    #[cold]
                    let _: Box<Node<T>> = Box::from_raw(node);
                }

                return ret;
            }
        }

        None
    }
}

impl<T> Drop for Entry<T> {
    // only call this drop in the same thread, or you must make sure it happens with no contension
    // running in a coroutine is a kind of sequential operation, so it can safely drop there after
    // returning from "kernel"
    fn drop(&mut self) {
        let node = unsafe { self.0.as_mut() };
        // dec the ref count of node
        node.refs -= 1;
        if node.refs == 0 {
            // release the node
            let _: Box<Node<T>> = unsafe { Box::from_raw(node) };
        }
    }
}

unsafe impl<T: Send> Send for Entry<T> {}

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
        // there is no handle for the node, so it's ref should be 1 now
        unsafe { &mut *stub }.refs = 1;
        Queue {
            head: AtomicPtr::new(stub).into(),
            tail: UnsafeCell::new(stub),
        }
    }

    /// Pushes a new value onto this queue.
    /// if the new node is head, indicate a true
    /// this is used to update the BH if it's a new head
    pub fn push(&self, t: T) -> (Entry<T>, bool) {
        unsafe {
            let node = Node::new(Some(t));
            let prev = self.head.swap(node, Ordering::AcqRel);
            (*node).prev = prev;
            (*prev).next.store(node, Ordering::Release);
            let tail = *self.tail.get();
            let is_head = tail == prev;
            (Entry(ptr::NonNull::new_unchecked(node)), is_head)
        }
    }

    /// if the queue is empty
    #[inline]
    pub fn is_empty(&self) -> bool {
        let tail = unsafe { *self.tail.get() };
        // the list is empty
        self.head.load(Ordering::Acquire) == tail
    }

    /// get the head ref
    #[inline]
    pub fn peek(&self) -> Option<&T> {
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
                if i > 500 {
                    thread::yield_now();
                    i = 0;
                }
            }

            assert!((*tail).value.is_none());
            assert!((*next).value.is_some());

            (*next).value.as_ref()
        }
    }

    pub fn pop_if<F>(&self, f: &F) -> Option<T>
    where
        F: Fn(&T) -> bool,
    {
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

            let v = (*next).value.as_ref().unwrap();
            if !f(v) {
                // no pop
                return None;
            }

            // clear the link bit
            assert!((*tail).refs & REF_COUNT_MASK != 0);
            (*tail).refs &= REF_COUNT_MASK;

            // clear the prev pointer indicate a new end point
            (*next).prev = ptr::null_mut();
            // move the tail to next
            *self.tail.get() = next;

            // we take the next value, this is why use option to host the value
            let ret = (*next).value.take().unwrap();
            (*tail).refs -= 1;
            if (*tail).refs == 0 {
                // release the node only when the ref count becomes 0
                let _: Box<Node<T>> = Box::from_raw(tail);
            }

            Some(ret)
        }
    }

    /// Pops some data from this queue.
    pub fn pop(&self) -> Option<T> {
        unsafe {
            let tail = *self.tail.get();

            // the list is empty
            if self.head.load(Ordering::Acquire) == tail {
                return None;
            }

            // clear the link bit
            assert!((*tail).refs & REF_COUNT_MASK != 0);
            (*tail).refs &= REF_COUNT_MASK;

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
            (*next).prev = ptr::null_mut();
            // move the tail to next
            *self.tail.get() = next;

            assert!((*tail).value.is_none());
            assert!((*next).value.is_some());
            // we tack the next value, this is why use option to host the value
            let ret = (*next).value.take().unwrap();
            (*tail).refs -= 1;
            if (*tail).refs == 0 {
                // release the node only when the ref count becomes 0
                let _: Box<Node<T>> = Box::from_raw(tail);
            }

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
        // release the stub
        let _: Box<Node<T>> = unsafe { Box::from_raw(*self.tail.get()) };
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
        let a = q.push(3);
        let b = q.push(4);
        assert_eq!(a.1, true);
        assert_eq!(a.0.remove(), Some(3));
        assert_eq!(b.1, false);
        assert_eq!(b.0.remove(), None);
        assert_eq!(q.pop(), Some(4));
        assert_eq!(q.is_empty(), true);

        q.push(5);
        q.push(6);
        q.push(7);
        let co = |v: &usize| *v < 7;
        assert_eq!(q.peek(), Some(&5));
        assert_eq!(q.pop_if(&co), Some(5));
        assert_eq!(q.pop_if(&co), Some(6));
        assert_eq!(q.pop_if(&co), None);
        assert_eq!(q.pop(), Some(7));
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
