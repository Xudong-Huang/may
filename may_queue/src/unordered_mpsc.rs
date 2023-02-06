use crossbeam_utils::{Backoff, CachePadded};
use smallvec::SmallVec;
use thread_local::ThreadLocal;

use crate::spsc::Queue as Ch;

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

#[derive(Debug)]
struct Channel<T> {
    ch: Ch<T>,
    next: AtomicPtr<Channel<T>>,
    detached: AtomicBool,
}

impl<T> Channel<T> {
    fn new() -> Self {
        Channel {
            ch: Ch::new(),
            next: AtomicPtr::new(std::ptr::null_mut()),
            detached: AtomicBool::new(true),
        }
    }
}

#[derive(Debug)]
pub struct Queue<T: Send> {
    head: CachePadded<AtomicPtr<Channel<T>>>,
    tail: UnsafeCell<*mut Channel<T>>,
    channels: ThreadLocal<Channel<T>>,
}

/// fast unordered mpsc queue when in highly push contention
/// each producer use its thread local spsc to push the element
/// the consumer would pop the element from spsc queue and switch
/// to another spsc queue when empty
impl<T: Send> Default for Queue<T> {
    fn default() -> Self {
        Self::new()
    }
}

unsafe impl<T: Send> Send for Queue<T> {}
unsafe impl<T: Send> Sync for Queue<T> {}

impl<T: Send> Queue<T> {
    pub fn new() -> Self {
        let channels = ThreadLocal::new();
        let ch = channels.get_or(|| Channel::new());
        ch.detached.store(false, Ordering::Relaxed);
        let node = ch as *const _ as *mut _;
        Queue {
            head: AtomicPtr::new(node).into(),
            tail: UnsafeCell::new(node),
            channels,
        }
    }

    #[inline]
    fn push_ch(&self, ch: &Channel<T>) {
        ch.next.store(std::ptr::null_mut(), Ordering::Relaxed);
        let node = ch as *const _ as *mut _;
        let prev = self.head.swap(node, Ordering::AcqRel);
        if prev != node {
            unsafe { (*prev).next.store(node, Ordering::Release) };
        }
    }

    // return the popped channel, and successfully popped result
    #[inline]
    fn pop_ch(&self) -> (&Channel<T>, bool) {
        let tail = unsafe { &mut **self.tail.get() };

        // the only node
        if self.head.load(Ordering::Acquire) == tail {
            return (tail, false);
        }

        // spin until tail next become non-null
        let backoff = Backoff::new();
        let mut next = tail.next.load(Ordering::Acquire);
        while next.is_null() {
            backoff.spin();
            next = tail.next.load(Ordering::Acquire);
        }

        // move the tail to next
        unsafe { *self.tail.get() = next };

        // return the removed node
        tail.detached.store(true, Ordering::Release);
        (tail, true)
    }

    #[inline]
    fn peek_ch(&self) -> &Channel<T> {
        unsafe { &**self.tail.get() }
    }

    pub fn push(&self, v: T) {
        let ch = self.channels.get_or(|| Channel::new());
        ch.ch.push(v);
        if ch.detached.swap(false, Ordering::Relaxed) {
            self.push_ch(ch);
        }
    }

    pub fn pop(&self) -> Option<T> {
        loop {
            let ch = self.peek_ch();
            match ch.ch.pop() {
                Some(v) => return Some(v),
                None => {
                    let (ch, popped) = self.pop_ch();
                    if !popped {
                        return None;
                    }
                    // re-check
                    if !ch.ch.is_empty() && ch.detached.swap(false, Ordering::Relaxed) {
                        self.push_ch(ch);
                    }
                }
            }
        }
    }

    pub fn bulk_pop(&self) -> SmallVec<[T; crate::spsc::BLOCK_SIZE]> {
        loop {
            let ch = self.peek_ch();
            let ret = ch.ch.bulk_pop();
            if !ret.is_empty() {
                return ret;
            } else {
                let (ch, popped) = self.pop_ch();
                if !popped {
                    return ret;
                }
                // re-check
                if !ch.ch.is_empty() && ch.detached.swap(false, Ordering::Relaxed) {
                    self.push_ch(ch);
                }
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        let ch = self.peek_ch();
        ch.next.load(Ordering::Acquire).is_null() && ch.ch.is_empty()
    }
}

#[cfg(all(nightly, test))]
mod test {
    extern crate test;
    use self::test::Bencher;
    use super::*;

    use std::sync::Arc;
    use std::thread;

    use crate::test_queue::ScBlockPop;

    impl<T: Send> ScBlockPop<T> for super::Queue<T> {
        fn block_pop(&self) -> T {
            let backoff = Backoff::new();
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
        assert!(!q.is_empty());

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

            let mut total = 0;
            for _i in 0..total_work {
                let v = q.block_pop();
                total += v;
            }

            assert_eq!(total, (0..total_work).sum())
        });
    }

    #[bench]
    fn multi_2p1c_test(b: &mut Bencher) {
        b.iter(|| {
            let q = Arc::new(Queue::new());
            let total_work: usize = 10_000_000;
            // create worker threads that generate mono increasing index
            // in other thread the value should be still 100
            let mut total = 0;
            let threads = 20;
            let barrier = Arc::new(std::sync::Barrier::new(threads + 1));

            thread::scope(|s| {
                for i in 0..threads {
                    let q = q.clone();
                    let c = barrier.clone();
                    s.spawn(move || {
                        c.wait();
                        let len = total_work / threads;
                        let start = i * len;
                        for v in start..start + len {
                            let _v = q.push(v);
                        }
                    });
                }
                s.spawn(|| {
                    barrier.wait();
                    for _ in 0..total_work {
                        total += q.block_pop();
                    }
                });
            });
            assert!(q.is_empty());
            assert_eq!(total, (0..total_work).sum::<usize>());
        });
    }

    #[bench]
    fn bulk_2p1c_test(b: &mut Bencher) {
        b.iter(|| {
            let q = Arc::new(Queue::new());
            let total_work: usize = 10_000_000;
            // create worker threads that generate mono increasing index
            // in other thread the value should be still 100
            let mut sum = 0;
            let threads = 20;
            let barrier = Arc::new(std::sync::Barrier::new(threads + 1));

            thread::scope(|s| {
                for i in 0..threads {
                    let q = q.clone();
                    let c = barrier.clone();
                    s.spawn(move || {
                        c.wait();
                        let len = total_work / threads;
                        let start = i * len;
                        for v in start..start + len {
                            let _v = q.push(v);
                        }
                    });
                }
                s.spawn(|| {
                    barrier.wait();
                    let mut total = 0;
                    while total < total_work {
                        let v = q.bulk_pop();
                        total += v.len();
                        for i in v {
                            sum += i;
                        }
                    }
                });
            });
            assert!(q.is_empty());
            assert_eq!(sum, (0..total_work).sum::<usize>());
        });
    }
}
