use std::cell::Cell;
#[cfg(feature = "work_steal")]
use std::cell::UnsafeCell;
use std::io;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
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
use may_queue::mpsc::Queue;

cfg_if::cfg_if! {
    if #[cfg(feature = "crossbeam_queue_steal")] {
        use crate::crossbeam_queue_shim::{self as spmc, Local, Steal};
    } else if #[cfg(feature = "work_steal")] {
        use may_queue::spmc::{self, Local, Steal};
    } else {
        use may_queue::spsc::Queue as Local;
    }
}

// thread id, only workers are normal ones
thread_local! { pub static WORKER_ID: Cell<usize> = const { Cell::new(usize::MAX) }; }

// here we use Arc<AtomicOption<>> for that in the select implementation
// other event may try to consume the coroutine while timer thread consume it
type TimerData = Arc<AtomicOption<CoroutineImpl>>;
type TimerThread = timeout_list::TimerThread<TimerData>;

// The scheduler is created once and never dropped, so it is leaked and
// published through an atomic pointer. This cannot be a plain static: other
// threads may read it on the fast path in `get_scheduler` while
// `init_scheduler` is still publishing it.
static SCHED: AtomicPtr<Scheduler> = AtomicPtr::new(std::ptr::null_mut());

/// Return the published scheduler, or `None` if initialization has not
/// completed yet.
#[inline]
fn load_scheduler() -> Option<&'static Scheduler> {
    let ptr = SCHED.load(Ordering::Acquire);
    if ptr.is_null() {
        None
    } else {
        // SAFETY: the pointer is only ever stored once, and it points to a
        // leaked `Scheduler` that lives for the rest of the process. The
        // Acquire load synchronizes with the Release store in
        // `init_scheduler`, so the Scheduler's contents are fully visible.
        Some(unsafe { &*ptr })
    }
}

#[cold]
fn init_scheduler() {
    let workers = config().get_workers();
    let b: Box<Scheduler> = Scheduler::new(workers);
    let sched: &'static Scheduler = Box::leak(b);
    SCHED.store(
        sched as *const Scheduler as *mut Scheduler,
        Ordering::Release,
    );

    // timer thread
    thread::spawn(move || {
        // timer function
        let timer_event_handler = |c: Arc<AtomicOption<CoroutineImpl>>| {
            // just re-push the co to the visit list
            if let Some(mut co) = c.take() {
                // set the timeout result for the coroutine
                set_co_para(&mut co, io::Error::new(io::ErrorKind::TimedOut, "timeout"));
                // s.schedule_global(c);
                run_coroutine(co);
            }
        };

        let s = load_scheduler().expect("scheduler published before spawn");
        s.timer_thread.run(&timer_event_handler);
    });

    let core_ids = core_affinity::get_core_ids().unwrap();
    let pin_cores = config().get_worker_pin();
    // io event loop thread
    for (id, core) in (0..workers).zip(core_ids.into_iter().cycle()) {
        thread::spawn(move || {
            if pin_cores {
                core_affinity::set_for_current(core);
            }
            let s = load_scheduler().expect("scheduler published before spawn");
            s.event_loop.run(id);
        });
    }
}

#[inline]
pub fn get_scheduler() -> &'static Scheduler {
    let ptr = SCHED.load(Ordering::Acquire);
    if likely(!ptr.is_null()) {
        // SAFETY: the pointer is stored once, points at a leaked Scheduler
        // that lives for the rest of the process, and this Acquire load
        // synchronizes with the Release store in `init_scheduler`.
        return unsafe { &*ptr };
    }
    static ONCE: Once = Once::new();
    ONCE.call_once(init_scheduler);
    // `call_once` synchronizes with the initializing thread, so the Release
    // store in `init_scheduler` is visible here.
    load_scheduler().expect("scheduler initialized by `call_once`")
}

#[repr(align(128))]
pub struct Scheduler {
    #[cfg(not(feature = "work_steal"))]
    local_queues: Vec<Local<CoroutineImpl>>,
    #[cfg(feature = "work_steal")]
    local_queues: Vec<UnsafeCell<Local<CoroutineImpl>>>,
    #[cfg(feature = "work_steal")]
    stealers: Vec<Steal<CoroutineImpl>>,
    global_queues: Vec<Queue<CoroutineImpl>>,
    event_loop: EventLoop,
    timer_thread: TimerThread,
    pub pool: CoroutinePool,
    pub workers: usize,
}

impl Scheduler {
    pub fn new(workers: usize) -> Box<Self> {
        #[cfg(not(feature = "work_steal"))]
        let local_queues = Vec::from_iter((0..workers).map(|_| Local::new()));

        #[cfg(feature = "work_steal")]
        let queues = Vec::from_iter((0..workers).map(|_| spmc::local()));
        #[cfg(feature = "work_steal")]
        let stealers = Vec::from_iter(queues.iter().map(|(s, _l)| s.clone()));
        #[cfg(feature = "work_steal")]
        let local_queues = Vec::from_iter(queues.into_iter().map(|(_s, l)| UnsafeCell::new(l)));

        let global_queues = Vec::from_iter((0..workers).map(|_| Queue::new()));

        Box::new(Scheduler {
            pool: CoroutinePool::new(),
            event_loop: EventLoop::new(workers).expect("can't create event_loop"),
            local_queues,
            #[cfg(feature = "work_steal")]
            stealers,
            global_queues,
            timer_thread: TimerThread::new(),
            workers,
        })
    }

    #[inline]
    #[cfg(not(feature = "work_steal"))]
    pub fn run_queued_tasks(&self, id: usize) {
        let local = unsafe { self.local_queues.get_unchecked(id) };
        while let Some(co) = local.pop() {
            run_coroutine(co);
        }
    }

    #[inline]
    #[cfg(feature = "work_steal")]
    pub fn run_queued_tasks(&self, id: usize) {
        let local = unsafe { &mut *self.local_queues.get_unchecked(id).get() };

        let max_steal: usize = std::cmp::min(3, self.workers - 1);

        #[cfg(feature = "rand_work_steal")]
        let mut rng = fastrand::Rng::new();

        'work: loop {
            match local.pop() {
                Some(co) => {
                    run_coroutine(co);
                    continue 'work;
                }
                None => {
                    self.collect_global(id);
                    if local.has_tasks() {
                        continue 'work;
                    }
                }
            }

            // The i variable is unused if rand_work_steal is enabled since it selects the steal target randomly instead.
            for _i in 0..max_steal {
                cfg_if::cfg_if! {
                    if #[cfg(feature = "rand_work_steal")] {
                        let target = rng.usize(0..self.workers);
                    } else {
                        let target = (id + _i + 1) % self.workers;
                    }
                };
                let stealer = self.stealers.get(target).unwrap();
                if let Some(co) = stealer.steal_into(local) {
                    run_coroutine(co);
                    continue 'work;
                }
            }
            return;
        }
    }

    /// put the coroutine to correct queue so that next time it can be scheduled
    #[inline]
    pub fn schedule(&self, co: CoroutineImpl) {
        let id = WORKER_ID.get();

        if id != usize::MAX {
            self.schedule_with_id(co, id);
        } else {
            self.schedule_global(co);
        }
    }

    /// called by selector with known id
    #[inline]
    #[cfg(feature = "work_steal")]
    pub fn schedule_with_id(&self, co: CoroutineImpl, id: usize) {
        let local = unsafe { &mut *self.local_queues.get_unchecked(id).get() };
        local.push_back(co);
    }

    #[inline]
    #[cfg(not(feature = "work_steal"))]
    pub fn schedule_with_id(&self, co: CoroutineImpl, id: usize) {
        let local = unsafe { self.local_queues.get_unchecked(id) };
        local.push(co);
    }

    /// put the coroutine to global queue so that next time it can be scheduled
    #[inline]
    pub fn schedule_global(&self, co: CoroutineImpl) {
        static NEXT_THREAD_ID: AtomicUsize = AtomicUsize::new(0);
        let thread_id = NEXT_THREAD_ID
            .fetch_add(1, Ordering::Relaxed)
            .rem_euclid(self.workers);
        let global = unsafe { self.global_queues.get_unchecked(thread_id) };
        global.push(co);
        // signal one waiting thread if any
        self.get_selector().wakeup(thread_id);
    }

    /// put the coroutine to global queue so that next time it can be scheduled
    #[inline]
    pub fn schedule_global_with_id(&self, co: CoroutineImpl, id: usize) {
        let thread_id = id.rem_euclid(self.workers);
        // println!("Scheduling to {thread_id}");
        let global = unsafe { self.global_queues.get_unchecked(thread_id) };
        global.push(co);
        // signal one waiting thread if any
        self.get_selector().wakeup(thread_id);
    }

    #[inline]
    pub fn collect_global(&self, id: usize) {
        #[cfg(feature = "work_steal")]
        let local = unsafe { &mut *self.local_queues.get_unchecked(id).get() };
        #[cfg(not(feature = "work_steal"))]
        let local = unsafe { self.local_queues.get_unchecked(id) };
        let global = unsafe { self.global_queues.get_unchecked(id) };
        let mut v = global.bulk_pop();
        while !v.is_empty() {
            for co in v {
                #[cfg(feature = "work_steal")]
                local.push_back(co);
                #[cfg(not(feature = "work_steal"))]
                local.push(co);
            }
            v = global.bulk_pop();
        }
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
