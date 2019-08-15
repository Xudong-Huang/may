use std::io;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Once};
use std::thread;
use std::time::Duration;

use crate::config::config;
use crate::coroutine_impl::{run_coroutine, CoroutineImpl};
use crate::io::{EventLoop, Selector};
use crate::pool::CoroutinePool;
use crate::sync::AtomicOption;
use crate::timeout_list;
use crate::yield_now::set_co_para;
use crossbeam::deque;
use crossbeam::utils::Backoff;

#[cfg(nightly)]
use std::intrinsics::likely;
#[cfg(not(nightly))]
#[inline]
fn likely(e: bool) -> bool {
    e
}

// thread id, only workers are normal ones
#[cfg(nightly)]
#[thread_local]
static WORKER_ID: AtomicUsize = AtomicUsize::new(!1);

#[cfg(not(nightly))]
thread_local! {static WORKER_ID: AtomicUsize = AtomicUsize::new(!1);}

// here we use Arc<AtomicOption<>> for that in the select implementation
// other event may try to consume the coroutine while timer thread consume it
type TimerData = Arc<AtomicOption<CoroutineImpl>>;
type TimerThread = timeout_list::TimerThread<TimerData>;

// filter out the cancel panic, don't print anything for it
fn filter_cancel_panic() {
    use generator::Error;
    use std::panic;
    let old = panic::take_hook();
    ::std::panic::set_hook(Box::new(move |info| {
        if let Some(&Error::Cancel) = info.payload().downcast_ref::<Error>() {
            // this is not an error at all, ignore it
            return;
        }
        old(info);
    }));
}

static mut SCHED: *const Scheduler = std::ptr::null();

struct WorkerThreads {
    parked: AtomicU64,
    threads: Vec<thread::Thread>,
}

impl WorkerThreads {
    fn new(workers: usize) -> Self {
        let parked = AtomicU64::new(0);
        let mut threads = Vec::with_capacity(workers);
        for _ in 0..workers {
            threads.push(thread::current()); // fake init
        }

        WorkerThreads { parked, threads }
    }

    fn wake_one(&self) {
        let parked = self.parked.load(Ordering::Relaxed);
        let first_thread = parked.trailing_zeros() as usize;
        if first_thread >= self.threads.len() {
            // there is no thread pending
            return;
        }
        let mask = 1 << first_thread;
        self.parked.fetch_and(!mask, Ordering::Relaxed);
        self.threads[first_thread].unpark();
    }

    fn get_thread_handle(&self, id: usize) -> &thread::Thread {
        unsafe { self.threads.get_unchecked(id) }
    }
}

#[cold]
#[inline(never)]
fn init_scheduler() {
    let workers = config().get_workers();
    let mut io_workers = config().get_io_workers();
    let run_on_io = if io_workers == 0 {
        // running all the coroutines on normal worker thread
        io_workers = 1;
        false
    } else {
        true
    };
    let b: Box<Scheduler> = Scheduler::new(workers, io_workers, run_on_io);
    unsafe {
        SCHED = Box::into_raw(b);
    }

    // run the workers in background
    for id in 0..workers {
        let handle = thread::spawn(move || {
            filter_cancel_panic();
            let s = unsafe { &*SCHED };
            s.run(id);
        })
        .thread()
        .clone();

        // set the real thread handle
        let s = unsafe { &mut *(SCHED as *const _ as *mut Scheduler) };
        s.workers.threads[id] = handle;
    }

    // timer thread
    thread::spawn(move || {
        filter_cancel_panic();
        let s = unsafe { &*SCHED };
        // timer function
        let timer_event_handler = |co: Arc<AtomicOption<CoroutineImpl>>| {
            // just re-push the co to the visit list
            if let Some(mut c) = co.take(Ordering::Relaxed) {
                // set the timeout result for the coroutine
                set_co_para(&mut c, io::Error::new(io::ErrorKind::TimedOut, "timeout"));
                s.schedule_global(c);
            }
        };

        s.timer_thread.run(&timer_event_handler);
    });

    // io event loop thread
    for id in 0..io_workers {
        thread::spawn(move || {
            filter_cancel_panic();
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

#[repr(align(128))]
pub struct Scheduler {
    pub pool: CoroutinePool,
    event_loop: EventLoop,
    global_queue: deque::Injector<CoroutineImpl>,
    local_queues: Vec<deque::Worker<CoroutineImpl>>,
    workers: WorkerThreads,
    timer_thread: TimerThread,
}

impl Scheduler {
    pub fn new(workers: usize, io_workers: usize, run_on_io: bool) -> Box<Self> {
        let mut local_queues = Vec::with_capacity(workers);
        (0..workers).for_each(|_| local_queues.push(deque::Worker::new_fifo()));
        Box::new(Scheduler {
            pool: CoroutinePool::new(),
            event_loop: EventLoop::new(io_workers, run_on_io).expect("can't create event_loop"),
            global_queue: deque::Injector::new(),
            local_queues,
            timer_thread: TimerThread::new(),
            workers: WorkerThreads::new(workers),
        })
    }

    fn run(&self, id: usize) {
        let mask = 1 << id;
        #[cfg(nightly)]
        WORKER_ID.store(id, Ordering::Relaxed);
        #[cfg(not(nightly))]
        WORKER_ID.with(|worker_id| worker_id.store(id, Ordering::Relaxed));

        let local = &self.local_queues[id];
        let mut stealers = Vec::new();
        for (i, worker) in self.local_queues.iter().enumerate() {
            if i != id {
                stealers.push(worker.stealer());
            }
        }
        stealers.rotate_left(id);

        fn steal_global<T>(global: &deque::Injector<T>, local: &deque::Worker<T>) -> Option<T> {
            let backoff = Backoff::new();
            loop {
                match global.steal_batch_and_pop(local) {
                    deque::Steal::Success(t) => return Some(t),
                    deque::Steal::Empty => return None,
                    deque::Steal::Retry => backoff.snooze(),
                }
            }
        };

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

        loop {
            // Pop a task from the local queue
            let co = local.pop().or_else(|| {
                // Try stealing a batch of tasks from the global queue.
                steal_global(&self.global_queue, local).or_else(|| {
                    // Try stealing a of task from other local queues.
                    stealers
                        .iter()
                        .map(|s| steal_local(s, local))
                        .find_map(|r| r)
                })
            });

            if let Some(co) = co {
                run_coroutine(co);
            } else {
                // first register thread handle
                self.workers.parked.fetch_or(mask, Ordering::Relaxed);

                // do a re-check
                if !self.global_queue.is_empty() {
                    self.workers.get_thread_handle(id).unpark()
                }

                // wake up every 500ms to check if there are more tasks
                thread::park_timeout(Duration::from_millis(500));
                // thread::park();

                // clear the park stat after comeback
                self.workers.parked.fetch_and(!mask, Ordering::Relaxed);
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
            self.local_queues[id].push(co);
        }
    }

    /// put the coroutine to global queue so that next time it can be scheduled
    #[inline]
    pub fn schedule_global(&self, co: CoroutineImpl) {
        self.global_queue.push(co);
        // signal one waiting thread if any
        self.workers.wake_one();
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
