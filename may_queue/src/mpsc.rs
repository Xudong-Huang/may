use crossbeam::utils::Backoff;
use smallvec::SmallVec;

use crate::spsc::Queue as Ch;

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, Ordering};

/// Read Processor ID
///
/// Reads the value of the IA32_TSC_AUX MSR (address C0000103H)
/// into the destination register.
///
/// # Unsafe
/// May fail with #UD if rdpid is not supported (check CPUID).
#[cfg(target_arch = "x86_64")]
#[inline(always)]
pub unsafe fn rdpid() -> usize {
    use std::arch::asm;
    let mut pid: usize;
    asm!("rdpid {}", out(reg) pid);
    pid
}

struct Channel<T> {
    ch: Ch<T>,
    lock: AtomicBool,
}

impl<T> Channel<T> {
    fn try_lock(&self) -> bool {
        self.lock
            .compare_exchange_weak(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    }

    fn unlock(&self) {
        self.lock.store(false, Ordering::Relaxed);
    }
}

pub struct Queue<T> {
    channels: Vec<Channel<T>>,
    cur_ch: UnsafeCell<usize>,
}

unsafe impl<T: Send> Send for Queue<T> {}
unsafe impl<T: Sync> Sync for Queue<T> {}

impl<T> Queue<T> {
    pub fn new() -> Self {
        let channels = (0..num_cpus::get())
            .map(|_| Channel {
                ch: Ch::new(),
                lock: AtomicBool::new(false),
            })
            .collect();
        Queue {
            channels,
            cur_ch: UnsafeCell::new(0),
        }
    }

    pub fn push(&self, v: T) {
        let backoff = Backoff::new();
        loop {
            let id = unsafe { rdpid() };
            let ch = unsafe { self.channels.get_unchecked(id) };
            if ch.try_lock() {
                ch.ch.push(v);
                ch.unlock();
                break;
            }

            backoff.snooze();
        }
    }

    pub fn pop(&self) -> Option<T> {
        let mut id = unsafe { *self.cur_ch.get() };
        let mut ch = unsafe { self.channels.get_unchecked(id) };
        let mut ch_switch = 0;
        while ch_switch < self.channels.len() {
            let v = ch.ch.pop();
            if v.is_some() {
                unsafe { *self.cur_ch.get() = id };
                return v;
            }

            ch_switch += 1;
            id = (id + 1) % self.channels.len();
            ch = unsafe { self.channels.get_unchecked(id) };
        }

        unsafe { *self.cur_ch.get() = id };
        None
    }

    pub fn bulk_pop(&self) -> Option<SmallVec<[T; crate::spsc::BLOCK_SIZE]>> {
        let mut id = unsafe { *self.cur_ch.get() };
        let mut ch = unsafe { self.channels.get_unchecked(id) };
        let mut ch_switch = 0;
        while ch_switch < self.channels.len() {
            let v = ch.ch.bulk_pop();
            if v.is_some() {
                unsafe { *self.cur_ch.get() = id };
                return v;
            }

            ch_switch += 1;
            id = (id + 1) % self.channels.len();
            ch = unsafe { self.channels.get_unchecked(id) };
        }

        unsafe { *self.cur_ch.get() = id };
        None
    }

    pub fn len(&self) -> usize {
        let mut len = 0;
        for ch in &self.channels {
            len += ch.ch.len();
        }
        len
    }

    pub fn is_empty(&self) -> bool {
        for ch in &self.channels {
            if ch.ch.len() > 0 {
                return false;
            }
        }
        true
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

    impl<T> ScBlockPop<T> for super::Queue<T> {
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
        assert_eq!(q.len(), 0);
        for i in 0..100 {
            q.push(i);
        }
        assert_eq!(q.len(), 100);

        for i in 0..100 {
            assert_eq!(q.pop(), Some(i));
        }
        assert_eq!(q.pop(), None);
        assert_eq!(q.len(), 0);
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
            let total_work: usize = 1_000_000;
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
            let total_work: usize = 1_000_000;
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
                        if let Some(v) = q.bulk_pop() {
                            total += v.len();
                            for i in v {
                                sum += i;
                            }
                        }
                    }
                });
            });
            assert!(q.is_empty());
            assert_eq!(sum, (0..total_work).sum::<usize>());
        });
    }
}
