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
    let pin_cores = config().get_worker_pin();
    // Fetch the core ids only when pinning is requested, and never fail the
    // whole startup over them: a missing affinity mask just means the workers
    // run unpinned.
    let core_ids = if pin_cores {
        match core_affinity::get_core_ids() {
            Some(ids) if !ids.is_empty() => Some(ids),
            _ => {
                warn!("can't get core ids, worker pinning disabled");
                None
            }
        }
    } else {
        None
    };

    // Keep ownership of the scheduler until every fallible startup step below
    // has completed. The new threads obtain it through `get_scheduler`, which
    // blocks on the `Once` the initializing thread is currently inside, so
    // nothing can observe the `SCHED` pointer before it is stored.
    let sched: Box<Scheduler> = Scheduler::new(workers);

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

        // wait until `init_scheduler` publishes the scheduler; if it panicked
        // this panics on the poisoned `Once` and the thread exits instead of
        // running against a half-initialized scheduler
        let s = get_scheduler();
        s.timer_thread.run(&timer_event_handler);
    });

    // io event loop thread
    for id in 0..workers {
        let core = core_ids.as_ref().map(|ids| ids[id % ids.len()]);
        thread::spawn(move || {
            if let Some(core) = core {
                core_affinity::set_for_current(core);
            }
            // see the timer thread above
            let s = get_scheduler();
            s.event_loop.run(id);
        });
    }

    // Publish only after all threads have been spawned. If a step above
    // panicked, `sched` is dropped during unwinding and `SCHED` stays null, so
    // later callers see the poisoned `Once` instead of a scheduler with
    // missing workers.
    SCHED.store(Box::into_raw(sched), Ordering::Release);
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

    /// Re-borrow the local queue of worker `id` from its `UnsafeCell`.
    ///
    /// The local queue is owned by the worker with this `id`: that thread
    /// pushes and pops, while other workers only [`Steal`] from it. Every
    /// access still goes through the `UnsafeCell`, so a fresh `&mut` has to be
    /// derived for each use and dropped before the next derivation. In
    /// particular it must not be held across `run_coroutine` (the coroutine
    /// may yield and re-enter `schedule_with_id` for this same `id`) or across
    /// `collect_global` (which derives its own borrow of the same cell).
    /// Stacked Borrows treats each re-derivation as invalidating the previous
    /// reference, so a long-lived `&mut` here would be UB.
    ///
    /// # Safety
    ///
    /// * Only the worker thread that owns `id` may access its local queue.
    /// * The returned reference must not be used after `local_queue` is called
    ///   again for the same `id`.
    #[inline]
    #[cfg(feature = "work_steal")]
    // the returned `&mut` comes out of an `UnsafeCell`, which is what gates
    // access; the safety contract above covers the aliasing rules
    #[allow(clippy::mut_from_ref)]
    unsafe fn local_queue(&self, id: usize) -> &mut Local<CoroutineImpl> {
        unsafe { &mut *self.local_queues.get_unchecked(id).get() }
    }

    #[inline]
    #[cfg(feature = "work_steal")]
    pub fn run_queued_tasks(&self, id: usize) {
        let max_steal: usize = std::cmp::min(3, self.workers - 1);

        #[cfg(feature = "rand_work_steal")]
        let mut rng = fastrand::Rng::new();

        'work: loop {
            // derive the borrow for this operation only, see `local_queue`
            let popped = unsafe { self.local_queue(id) }.pop();
            match popped {
                Some(co) => {
                    run_coroutine(co);
                    continue 'work;
                }
                None => {
                    self.collect_global(id);
                    if unsafe { self.local_queue(id) }.has_tasks() {
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
                let stolen = stealer.steal_into(unsafe { self.local_queue(id) });
                if let Some(co) = stolen {
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
        // SAFETY: this runs on the worker owning `id`, and the borrow ends
        // with the push, before any coroutine code can run.
        unsafe { self.local_queue(id) }.push_back(co);
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
        // SAFETY: this runs on the worker owning `id`; `bulk_pop` below never
        // re-enters the scheduler, so the borrow is not invalidated here.
        let local = unsafe { self.local_queue(id) };
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
