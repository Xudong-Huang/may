use crate::atomic::AtomicUsize;
use crate::spsc::Queue as Ch;

use std::sync::atomic::{AtomicBool, Ordering};

struct Channel<T> {
    ch: Ch<T>,
    lock: AtomicBool,
}

impl<T> Channel<T> {
    fn try_lock(&self) -> bool {
        self.lock
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn unlock(&self) {
        self.lock.store(false, Ordering::Release);
    }
}

const MAX_DEPTH: usize = 32;
pub struct Queue<T> {
    channels: Vec<Channel<T>>,
    cur_ch: AtomicUsize,
    cur_depth: AtomicUsize,
    // lock: AtomicBool,
    // global_ch: Ch<T>,
    push_index: AtomicUsize,
    pop_index: AtomicUsize,
}

impl<T> Queue<T> {
    // fn try_lock(&self) -> bool {
    //     self.lock
    //         .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
    //         .is_ok()
    // }

    // fn unlock(&self) {
    //     self.lock.store(false, Ordering::Release);
    // }

    pub fn new() -> Self {
        let channels = (0..num_cpus::get())
            .map(|_| Channel {
                ch: Ch::new(),
                lock: AtomicBool::new(false),
            })
            .collect();
        Queue {
            channels,
            cur_ch: AtomicUsize::new(0),
            cur_depth: AtomicUsize::new(0),
            push_index: AtomicUsize::new(0),
            pop_index: AtomicUsize::new(0),
            // lock: AtomicBool::new(false),
            // global_ch: Ch::new(),
        }
    }

    pub fn push(&self, v: T) {
        loop {
            // if self.try_lock() {
            //     self.global_ch.push(v);
            //     self.unlock();
            //     break;
            // } else {
            let id = unsafe { libc::sched_getcpu() } as usize;
            let ch = unsafe { self.channels.get_unchecked(id) };
            if ch.try_lock() {
                ch.ch.push(v);
                ch.unlock();
                break;
            } else {
                std::thread::yield_now();
            }
            // }
        }
        self.push_index.fetch_add(1, Ordering::Release);
    }

    pub fn pop(&self) -> Option<T> {
        let mut id = self.cur_ch.load(Ordering::Acquire);
        let mut ch = unsafe { self.channels.get_unchecked(id) };
        // let mut ch_switch = 0;
        while self.len() > 0 {
            // if ch_switch % self.channels.len() == 0 {
            //     if let Some(v) = self.global_ch.pop() {
            //         let pop_index = self.pop_index.load(Ordering::Relaxed);
            //         self.pop_index.store(pop_index + 1, Ordering::Relaxed);
            //         return Some(v);
            //     }
            // }

            let cur_depth = unsafe { self.cur_depth.unsync_load() };
            if cur_depth < MAX_DEPTH {
                let v = ch.ch.pop();
                if v.is_some() {
                    let pop_index = self.pop_index.load(Ordering::Relaxed);
                    self.pop_index.store(pop_index + 1, Ordering::Relaxed);
                    self.cur_depth.store(cur_depth + 1, Ordering::Relaxed);
                    return v;
                }
            }

            // ch_switch += 1;
            id = (id + 1) % self.channels.len();
            ch = unsafe { self.channels.get_unchecked(id) };

            self.cur_ch.store(id, Ordering::Relaxed);
            self.cur_depth.store(0, Ordering::Relaxed);
        }

        None
    }

    pub fn len(&self) -> usize {
        self.push_index.load(Ordering::Acquire) - self.pop_index.load(Ordering::Acquire)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
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
