use std::cell::UnsafeCell;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

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

unsafe impl<T: Send> Send for Queue<T> {}
unsafe impl<T: Send> Sync for Queue<T> {}

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
            let backoff = crossbeam::utils::Backoff::new();
            loop {
                next = (*tail).next.load(Ordering::Acquire);
                if !next.is_null() {
                    break;
                }
                backoff.snooze();
            }
            // value is not an atomic operation it may read out old shadow value
            // assert!((*tail).value.is_none());
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
        while self.pop().is_some() {}
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
        assert!(q.is_empty());
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

#[cfg(all(nightly, test))]
mod bench {
    extern crate test;
    use self::test::Bencher;
    use super::*;

    use std::sync::Arc;
    use std::thread;

    use crate::test_queue::ScBlockPop;

    impl<T: Send> ScBlockPop<T> for super::Queue<T> {
        fn block_pop(&self) -> T {
            let backoff = crossbeam::utils::Backoff::new();
            loop {
                match self.pop() {
                    Some(v) => return v,
                    None => backoff.snooze(),
                }
            }
        }
    }

    #[test]
    fn queue_sanity() {
        let q = Queue::<usize>::new();
        assert!(q.is_empty());
        for i in 0..100 {
            q.push(i);
        }
        // assert_eq!(q.len(), 100);
        // println!("{q:?}");

        for i in 0..100 {
            assert_eq!(q.pop(), Some(i));
        }
        assert_eq!(q.pop(), None);
        assert!(q.is_empty());
    }

    #[bench]
    fn single_thread_test(b: &mut Bencher) {
        let q = Queue::new();
        let mut i = 0;
        b.iter(|| {
            q.push(i);
            assert_eq!(q.pop(), Some(i));
            i += 1;
        });
    }

    #[bench]
    fn multi_1p1c_test(b: &mut Bencher) {
        b.iter(|| {
            let q = Arc::new(Queue::new());
            let total_work: usize = 1_000_000;
            // create worker threads that generate mono increasing index
            let _q = q.clone();
            // in other thread the value should be still 100
            thread::spawn(move || {
                for i in 0..total_work {
                    _q.push(i);
                }
            });

            for i in 0..total_work {
                let v = q.block_pop();
                assert_eq!(i, v);
            }
        });
    }

    #[bench]
    fn multi_2p1c_test(b: &mut Bencher) {
        b.iter(|| {
            let q = Arc::new(Queue::new());
            let total_work: usize = 1_000_000;
            // create worker threads that generate mono increasing index
            // in other thread the value should be still 100
            let mut total = 0;

            thread::scope(|s| {
                let threads = 20;
                for i in 0..threads {
                    let q = q.clone();
                    s.spawn(move || {
                        let len = total_work / threads;
                        let start = i * len;
                        for v in start..start + len {
                            let _v = q.push(v);
                        }
                    });
                }
                s.spawn(|| {
                    for _ in 0..total_work {
                        total += q.block_pop();
                    }
                });
            });
            assert!(q.is_empty());
            assert_eq!(total, (0..total_work).sum::<usize>());
        });
    }

    // #[bench]
    // fn bulk_1p2c_test(b: &mut Bencher) {
    //     b.iter(|| {
    //         let q = Arc::new(Queue::new());
    //         let total_work: usize = 1_000_000;
    //         // create worker threads that generate mono increasing index
    //         // in other thread the value should be still 100
    //         for i in 0..total_work {
    //             q.push(i);
    //         }

    //         let total = Arc::new(AtomicUsize::new(0));

    //         thread::scope(|s| {
    //             let threads = 20;
    //             for _ in 0..threads {
    //                 let q = q.clone();
    //                 let total = total.clone();
    //                 s.spawn(move || {
    //                     while !q.is_empty() {
    //                         if let Some(v) = q.bulk_pop() {
    //                             total.fetch_add(v.len(), Ordering::AcqRel);
    //                         }
    //                     }
    //                 });
    //             }
    //         });
    //         assert!(q.is_empty());
    //         assert_eq!(total.load(Ordering::Acquire), total_work);
    //     });
    // }
}
