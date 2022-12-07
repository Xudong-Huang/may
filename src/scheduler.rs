use std::io;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Once};
use std::thread;
use std::time::Duration;

use crate::config::config;
use crate::coroutine_impl::{run_coroutine, CoroutineImpl};
use crate::io::{EventLoop, Selector};
use crate::likely::likely;
use crate::pool::CoroutinePool;
use crate::sync::AtomicOption;
use crate::timeout_list;
use crate::yield_now::set_co_para;

use crossbeam::deque;
use crossbeam::queue::SegQueue;

// thread id, only workers are normal ones
#[cfg(nightly)]
#[thread_local]
#[no_mangle]
pub static WORKER_ID: AtomicUsize = AtomicUsize::new(!1);

#[cfg(not(nightly))]
thread_local! { pub static WORKER_ID: AtomicUsize = AtomicUsize::new(!1); }

// here we use Arc<AtomicOption<>> for that in the select implementation
// other event may try to consume the coroutine while timer thread consume it
type TimerData = Arc<AtomicOption<CoroutineImpl>>;
type TimerThread = timeout_list::TimerThread<TimerData>;

static mut SCHED: *const Scheduler = std::ptr::null();

pub struct ParkStatus {
    pub parked: AtomicU64,
    // workers: usize,
}

impl ParkStatus {
    fn new(workers: usize) -> Self {
        assert!(workers <= 64);
        let parked = AtomicU64::new(((1u128 << workers) - 1) as u64);
        // ParkStatus { parked, workers }
        ParkStatus { parked }
    }

    // #[inline]
    // fn get_idle_thread(&self) -> usize {
    //     // when the worker thread is idle, the corresponding bit would set to 1
    //     let parked = self.parked.load(Ordering::Relaxed);
    //     // find the right most set bit
    //     let rms = parked & !parked.wrapping_sub(1);
    //     let first_thread = rms.trailing_zeros() as usize;
    //     // if all threads are busy, we would not send any signal to wake up
    //     // any worker thread. In case worker thread missing the signal it will
    //     // wake up itself every 1 second, this is a rarely case
    //     if first_thread < self.workers {
    //         first_thread
    //     } else {
    //         0
    //     }
    // }

    #[inline]
    fn wake_one(&self, thread_id: usize, scheduler: &Scheduler) {
        // mark the thread as busy in advance (clear to 0)
        // the worker thread would set it to 1 when idle
        let mask = 1 << thread_id;
        self.parked.fetch_and(!mask, Ordering::Relaxed);
        scheduler.get_selector().wakeup(thread_id);
    }
}

#[inline(never)]
fn init_scheduler() {
    let workers = config().get_workers();
    let b: Box<Scheduler> = Scheduler::new(workers);
    unsafe {
        SCHED = Box::into_raw(b);
    }

    // timer thread
    thread::spawn(move || {
        // timer function
        let timer_event_handler = |c: Arc<AtomicOption<CoroutineImpl>>| {
            // just re-push the co to the visit list
            if let Some(mut co) = c.take(Ordering::Relaxed) {
                // set the timeout result for the coroutine
                set_co_para(&mut co, io::Error::new(io::ErrorKind::TimedOut, "timeout"));
                // s.schedule_global(c);
                run_coroutine(co);
            }
        };

        let s = unsafe { &*SCHED };
        s.timer_thread.run(&timer_event_handler);
    });

    // io event loop thread
    for id in 0..workers {
        thread::spawn(move || {
            let s = unsafe { &*SCHED };
            s.event_loop.run(id).unwrap_or_else(|e| {
                panic!("event_loop failed running, err={}", e);
            });
        });
    }
}

#[inline]
pub fn get_scheduler() -> &'static Scheduler {
    unsafe {
        if likely(!SCHED.is_null()) {
            return &*SCHED;
        }
    }
    static ONCE: Once = Once::new();
    ONCE.call_once(init_scheduler);
    unsafe { &*SCHED }
}

#[inline]
fn steal_local<T>(stealer: &deque::Stealer<T>, local: &deque::Worker<T>) -> Option<T> {
    match stealer.steal_batch_and_pop(local) {
        deque::Steal::Success(t) => Some(t),
        _ => None,
    }
}

#[repr(align(128))]
pub struct Scheduler {
    pub(crate) workers: ParkStatus,
    local_queues: Vec<deque::Worker<CoroutineImpl>>,
    global_queues: Vec<SegQueue<CoroutineImpl>>,
    stealers: Vec<deque::Stealer<CoroutineImpl>>,
    event_loop: EventLoop,
    timer_thread: TimerThread,
    pub pool: CoroutinePool,
}

impl Scheduler {
    pub fn new(workers: usize) -> Box<Self> {
        let mut local_queues = Vec::with_capacity(workers);
        (0..workers).for_each(|_| local_queues.push(deque::Worker::new_fifo()));
        let stealers = local_queues.iter().map(|l| l.stealer()).collect();
        let global_queues = (0..workers).map(|_| SegQueue::new()).collect();

        Box::new(Scheduler {
            pool: CoroutinePool::new(),
            event_loop: EventLoop::new(workers).expect("can't create event_loop"),
            local_queues,
            global_queues,
            stealers,
            timer_thread: TimerThread::new(),
            workers: ParkStatus::new(workers),
        })
    }

    #[inline]
    pub fn run_queued_tasks(&self, id: usize) {
        let local = unsafe { self.local_queues.get_unchecked(id) };
        let global = unsafe { self.global_queues.get_unchecked(id) };
        let mut steal_local_flag = false;

        let mut get_co = || {
            // Try get a task from the local queue.
            local
                .pop()
                // Try get a task from the global queue.
                .or_else(|| global.pop())
                // Try stealing a of task from other local queues.
                .or_else(|| {
                    if !steal_local_flag {
                        steal_local_flag = true;

                        let parked_threads = self.workers.parked.load(Ordering::Relaxed);

                        let mut next_steal_id = 1;
                        let next_id = loop {
                            let next_id = (id + next_steal_id) % self.stealers.len();
                            if parked_threads & (1 << next_id) == 0 {
                                break next_id; // at least we could stop by self bit
                            }
                            next_steal_id += 1;
                        };

                        let stealer = unsafe { self.stealers.get_unchecked(next_id) };
                        steal_local(stealer, &local)
                    } else {
                        None
                    }
                })
        };

        // Pop a task from the local queue
        let mut cur_co = get_co();

        if let Some(co) = &cur_co {
            co.prefetch();
        } else {
            return;
        }

        loop {
            // Pop a task from the local queue
            let next_co = get_co();

            if let Some(co) = cur_co {
                if let Some(next) = &next_co {
                    next.prefetch();
                }
                run_coroutine(co);
                cur_co = next_co;
            } else if let Some(next) = &next_co {
                next.prefetch();
                cur_co = next_co;
            } else {
                break;
            }
        }
    }

    /// put the coroutine to correct queue so that next time it can be scheduled
    #[inline]
    pub fn schedule(&self, co: CoroutineImpl) {
        #[cfg(nightly)]
        let id = WORKER_ID.load(Ordering::Relaxed);
        #[cfg(not(nightly))]
        let id = WORKER_ID.with(|id| id.load(Ordering::Relaxed));

        if id != !1 {
            unsafe { self.local_queues.get_unchecked(id) }.push(co);
        } else {
            self.schedule_global(co);
        }
    }

    /// put the coroutine to global queue so that next time it can be scheduled
    #[inline]
    pub fn schedule_global(&self, co: CoroutineImpl) {
        // let thread_id = self.workers.get_idle_thread();
        static NEXT_THREAD_ID: AtomicUsize = AtomicUsize::new(0);
        let thread_id = NEXT_THREAD_ID.fetch_add(1, Ordering::Relaxed) % self.global_queues.len();
        let global_queue = unsafe { self.global_queues.get_unchecked(thread_id) };
        global_queue.push(co);
        // signal one waiting thread if any
        self.workers.wake_one(thread_id, self);
    }

    #[inline]
    pub fn add_timer(
        &self,
        dur: Duration,
        co: Arc<AtomicOption<CoroutineImpl>>,
    ) -> timeout_list::TimeoutHandle<TimerData> {
        self.timer_thread.add_timer(dur, co)
    }

    #[inline]
    pub fn del_timer(&self, handle: timeout_list::TimeoutHandle<TimerData>) {
        self.timer_thread.del_timer(handle);
    }

    #[inline]
    pub fn get_selector(&self) -> &Selector {
        self.event_loop.get_selector()
    }
}
