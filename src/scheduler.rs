use std::cell::Cell;
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
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

use crate::sync::seg_queue::SegQueue;

// thread id, only workers are normal ones
#[cfg(nightly)]
#[thread_local]
#[no_mangle]
pub static WORKER_ID: Cell<usize> = Cell::new(!1);

#[cfg(not(nightly))]
thread_local! { pub static WORKER_ID: Cell<usize> = Cell::new(!1); }

// here we use Arc<AtomicOption<>> for that in the select implementation
// other event may try to consume the coroutine while timer thread consume it
type TimerData = Arc<AtomicOption<CoroutineImpl>>;
type TimerThread = timeout_list::TimerThread<TimerData>;

static mut SCHED: *const Scheduler = std::ptr::null();

#[inline(never)]
fn init_scheduler() {
    let workers = config().get_workers();
    let b: Box<Scheduler> = Scheduler::new(workers);
    unsafe { SCHED = Box::into_raw(b) };

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

    let core_ids = core_affinity::get_core_ids().unwrap();
    // io event loop thread
    for (id, core) in (0..workers).zip(core_ids.into_iter().cycle()) {
        thread::spawn(move || {
            core_affinity::set_for_current(core);
            let s = unsafe { &*SCHED };
            s.event_loop.run(id);
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
    local_queues: Vec<SegQueue<CoroutineImpl>>,
    event_loop: EventLoop,
    timer_thread: TimerThread,
    pub pool: CoroutinePool,
}

impl Scheduler {
    pub fn new(workers: usize) -> Box<Self> {
        let local_queues = Vec::from_iter((0..workers).map(|_| SegQueue::new()));

        Box::new(Scheduler {
            pool: CoroutinePool::new(),
            event_loop: EventLoop::new(workers).expect("can't create event_loop"),
            local_queues,
            timer_thread: TimerThread::new(),
        })
    }

    #[inline]
    pub fn run_queued_tasks(&self, id: usize) {
        let local = unsafe { self.local_queues.get_unchecked(id) };

        let mut next_id = id;

        let mut get_group = || {
            local
                // Try get a task from the local queue.
                .pop_bulk()
                // Try stealing a of task from other local queues.
                .or_else(|| {
                    next_id = (next_id + 1).rem_euclid(self.local_queues.len());
                    let stealer = unsafe { self.local_queues.get_unchecked(next_id) };
                    stealer.pop_bulk()
                })
        };

        // Pop a task from the local queue
        let mut cur_group = get_group();

        loop {
            if let Some(mut group) = cur_group {
                while let Some(cur_co) = group.pop() {
                    if let Some(next_c) = group.last() {
                        next_c.prefetch();
                    }
                    run_coroutine(cur_co);
                }
                cur_group = get_group();
            } else {
                cur_group = get_group();
                if cur_group.is_none() {
                    break;
                }
            }
        }
    }

    /// put the coroutine to correct queue so that next time it can be scheduled
    #[inline]
    pub fn schedule(&self, co: CoroutineImpl) {
        #[cfg(nightly)]
        let id = WORKER_ID.get();
        #[cfg(not(nightly))]
        let id = WORKER_ID.with(|id| id.get());

        if id != !1 {
            self.schedule_with_id(co, id);
        } else {
            self.schedule_global(co);
        }
    }

    /// called by selector with known id
    #[inline]
    pub fn schedule_with_id(&self, co: CoroutineImpl, id: usize) {
        let queue = unsafe { self.local_queues.get_unchecked(id) };
        queue.push(co);
    }

    /// put the coroutine to global queue so that next time it can be scheduled
    #[inline]
    pub fn schedule_global(&self, co: CoroutineImpl) {
        // let thread_id = self.workers.get_idle_thread();
        static NEXT_THREAD_ID: AtomicUsize = AtomicUsize::new(0);
        let thread_id = NEXT_THREAD_ID
            .fetch_add(1, Ordering::AcqRel)
            .rem_euclid(self.local_queues.len());
        let global = unsafe { self.local_queues.get_unchecked(thread_id) };
        global.push(co);
        // signal one waiting thread if any
        self.get_selector().wakeup(thread_id);
    }

    #[inline]
    pub fn collect_global(&self, _id: usize) {}

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
