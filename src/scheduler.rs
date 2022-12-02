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
use crossbeam::utils::Backoff;

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
    workers: usize,
}

impl ParkStatus {
    fn new(workers: usize) -> Self {
        assert!(workers <= 64);
        let parked = AtomicU64::new(((1u128 << workers) - 1) as u64);
        ParkStatus { parked, workers }
    }

    #[inline]
    fn wake_one(&self, scheduler: &Scheduler) {
        // when the worker thread is idle, the corresponding bit would set to 1
        let parked = self.parked.load(Ordering::Relaxed);
        // find the right most set bit
        let rms = parked & !parked.wrapping_sub(1);
        let first_thread = rms.trailing_zeros() as usize;
        // if all threads are busy, we would not send any signal to wake up
        // any worker thread. In case worker thread missing the signal it will
        // wake up itself every 1 second, this is a rarely case
        if first_thread < self.workers {
            // mark the thread as busy in advance (clear to 0)
            // the worker thread would set it to 1 when idle
            let mask = 1 << first_thread;
            self.parked.fetch_and(!mask, Ordering::Relaxed);
            scheduler.get_selector().wakeup(first_thread);
        }
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
        let s = unsafe { &*SCHED };
        // timer function
        let timer_event_handler = |co: Arc<AtomicOption<CoroutineImpl>>| {
            // just re-push the co to the visit list
            if let Some(mut c) = co.take(Ordering::Relaxed) {
                // set the timeout result for the coroutine
                set_co_para(&mut c, io::Error::new(io::ErrorKind::TimedOut, "timeout"));
                // s.schedule_global(c);
                run_coroutine(c);
            }
        };

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
fn steal_global<T>(global: &deque::Injector<T>, local: &deque::Worker<T>) -> Option<T> {
    static GLOBAL_LOCK: AtomicUsize = AtomicUsize::new(0);
    GLOBAL_LOCK
        .compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed)
        .ok()?;

    let backoff = Backoff::new();
    let ret = loop {
        match global.steal_batch_and_pop(local) {
            deque::Steal::Success(t) => break Some(t),
            deque::Steal::Empty => break None,
            deque::Steal::Retry => backoff.snooze(),
        }
    };
    GLOBAL_LOCK.store(0, Ordering::Relaxed);
    ret
}

#[inline]
fn steal_local<T>(stealer: &deque::Stealer<T>, local: &deque::Worker<T>) -> Option<T> {
    let backoff = Backoff::new();
    loop {
        match stealer.steal_batch_and_pop(local) {
            deque::Steal::Success(t) => return Some(t),
            deque::Steal::Empty => return None,
            deque::Steal::Retry => backoff.snooze(),
        }
    }
}

#[repr(align(128))]
pub struct Scheduler {
    pub pool: CoroutinePool,
    event_loop: EventLoop,
    global_queue: deque::Injector<CoroutineImpl>,
    local_queues: Vec<deque::Worker<CoroutineImpl>>,
    pub(crate) workers: ParkStatus,
    timer_thread: TimerThread,
    stealers: Vec<Vec<(usize, deque::Stealer<CoroutineImpl>)>>,
}

impl Scheduler {
    pub fn new(workers: usize) -> Box<Self> {
        let mut local_queues = Vec::with_capacity(workers);
        (0..workers).for_each(|_| local_queues.push(deque::Worker::new_fifo()));
        let mut stealers = Vec::with_capacity(workers);
        for id in 0..workers {
            let mut stealers_l = Vec::with_capacity(workers);
            for (i, worker) in local_queues.iter().enumerate() {
                if i != id {
                    stealers_l.push((i, worker.stealer()));
                }
            }
            stealers_l.rotate_left(id);
            stealers.push(stealers_l);
        }
        Box::new(Scheduler {
            pool: CoroutinePool::new(),
            event_loop: EventLoop::new(workers).expect("can't create event_loop"),
            global_queue: deque::Injector::new(),
            local_queues,
            timer_thread: TimerThread::new(),
            workers: ParkStatus::new(workers),
            stealers,
        })
    }

    pub fn run_queued_tasks(&self, id: usize) {
        let local = unsafe { self.local_queues.get_unchecked(id) };
        let stealers = unsafe { self.stealers.get_unchecked(id) };

        let get_co = || {
            local.pop().or_else(|| {
                // Try stealing a of task from other local queues.
                let parked_threads = self.workers.parked.load(Ordering::Relaxed);
                stealers
                    .iter()
                    .find_map(|s| {
                        if parked_threads & (1 << s.0) != 0 {
                            return None;
                        }
                        steal_local(&s.1, local)
                    })
                    // Try stealing a batch of tasks from the global queue.
                    .or_else(|| steal_global(&self.global_queue, local))
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
            } else if self.global_queue.is_empty() {
                // do a re-check
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

        if id == !1 {
            self.schedule_global(co);
        } else {
            unsafe { self.local_queues.get_unchecked(id) }.push(co);
        }
    }

    /// put the coroutine to global queue so that next time it can be scheduled
    #[inline]
    pub fn schedule_global(&self, co: CoroutineImpl) {
        self.global_queue.push(co);
        // signal one waiting thread if any
        self.workers.wake_one(self);
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
