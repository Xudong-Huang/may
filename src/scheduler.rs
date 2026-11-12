use std::cell::Cell;
#[cfg(feature = "work_steal")]
use std::cell::UnsafeCell;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
use std::sync::Once;
use std::thread;
use std::time::Duration;

use crossbeam::utils::CachePadded;

use crate::config::config;
use crate::coroutine_impl::{run_coroutine, CoroutineImpl};
use crate::io::{EventLoop, Selector};
use crate::likely::likely;
use crate::pool::CoroutinePool;
use crate::sync::WaitCo;
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

// the timer holds a per-wait handle, since in the select implementation
// another event may try to consume the coroutine while the timer thread does
type TimerThread = timeout_list::TimerThread<WaitCo>;

/// Maximum number of coroutines a worker runs from its local queue in one
/// drain before it returns to the selector.
///
/// A coroutine that keeps re-queueing itself locally (a `yield_now` loop, or a
/// wake-up loop through the sync primitives) would otherwise keep the drain
/// loop busy forever. The worker would never re-enter `select`, so the io
/// completions and io timers bound to its selector could never be serviced;
/// with a single worker that is a deadlock. The value trades batching (fewer
/// selector round-trips) against reactor latency; it is in the same range as
/// tokio's default `event_interval`.
const LOCAL_TASK_BUDGET: usize = 64;

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
        let timer_event_handler = |c: WaitCo| {
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
        loop {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                s.timer_thread.run(&timer_event_handler);
            }));
            if let Err(panic_info) = result {
                error!("timer thread panicked, restarting: {:?}", panic_info);
                // continue the loop to restart the timer thread
            } else {
                // normal exit (should not happen as timer_thread.run is infinite)
                break;
            }
        }
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
            loop {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    s.event_loop.run(id);
                }));
                if let Err(panic_info) = result {
                    error!(
                        "worker thread {} panicked, restarting: {:?}",
                        id, panic_info
                    );
                    // continue the loop to restart the event loop
                } else {
                    // normal exit (should not happen as event_loop.run is infinite)
                    break;
                }
            }
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
    /// One per worker: `true` while a wakeup for that worker is in flight, or
    /// until the worker disarms it right before going to sleep.
    ///
    /// Producers only issue the selector wakeup on the `false -> true`
    /// transition, so a burst of global pushes costs at most one syscall per
    /// worker instead of one per push. Cache padded because producers on other
    /// threads hammer these while the owning worker is trying to sleep.
    wakeup_armed: Vec<CachePadded<AtomicBool>>,
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
        let wakeup_armed =
            Vec::from_iter((0..workers).map(|_| CachePadded::new(AtomicBool::new(false))));

        Box::new(Scheduler {
            pool: CoroutinePool::new(),
            event_loop: EventLoop::new(workers).expect("can't create event_loop"),
            local_queues,
            #[cfg(feature = "work_steal")]
            stealers,
            global_queues,
            wakeup_armed,
            timer_thread: TimerThread::new(),
            workers,
        })
    }

    #[inline]
    #[cfg(not(feature = "work_steal"))]
    pub fn run_queued_tasks(&self, id: usize) {
        let local = unsafe { self.local_queues.get_unchecked(id) };
        for _ in 0..LOCAL_TASK_BUDGET {
            match local.pop() {
                Some(co) => run_coroutine(co),
                None => return,
            }
        }
        // Anything left in the queue is reported by `has_local_tasks`, which
        // makes the event loop poll the selector instead of blocking; this
        // worker then resumes draining after one non-blocking reactor round.
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
        // see `LOCAL_TASK_BUDGET`: bound the drain so the worker returns to
        // `select` even when coroutines keep re-queueing themselves locally
        let mut remaining = LOCAL_TASK_BUDGET;

        // Number of other workers; a single worker has nothing to steal from.
        let targets = self.workers - 1;

        'work: loop {
            if remaining == 0 {
                // Anything left in the queue is reported by
                // `has_local_tasks`, which makes the event loop poll the
                // selector instead of blocking; this worker then resumes
                // draining after one non-blocking reactor round.
                return;
            }

            // derive the borrow for this operation only, see `local_queue`
            let popped = unsafe { self.local_queue(id) }.pop();
            match popped {
                Some(co) => {
                    remaining -= 1;
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

            if targets == 0 {
                return;
            }

            // Visit every other worker once, starting at a random offset.
            //
            // The exhaustive sweep is what keeps the scheduler
            // work-conserving: a bounded scan could retry the same victims
            // forever while the only busy queue sat outside the window, and
            // a local enqueue does not wake a worker parked in `select`.
            // The random start only varies the probe order - it keeps
            // concurrent thieves from herding on the same victim and spreads
            // theft around the worker set; every victim is still visited
            // before this worker sleeps.
            //
            // A failed probe is three acquire loads and no RMW, and the
            // queues of idle workers are not written, so a sweep of empty
            // queues is cheap. `steal_into` moves a whole block, so one
            // successful probe is enough to keep this worker busy.
            let start = fastrand::usize(0..targets);
            for k in 0..targets {
                // compacted victim index in `0..targets`, mapped to a worker
                // id by skipping our own
                let v = (start + k) % targets;
                let target = if v >= id { v + 1 } else { v };
                let stealer = self.stealers.get(target).unwrap();
                let stolen = stealer.steal_into(unsafe { self.local_queue(id) });
                if let Some(co) = stolen {
                    remaining -= 1;
                    run_coroutine(co);
                    continue 'work;
                }
            }
            return;
        }
    }

    /// Whether worker `id` still has coroutines in its local run queue.
    ///
    /// The event loop calls this after `select` returns. A budgeted drain may
    /// stop while local work remains, and a local enqueue does not wake the
    /// selector; this tells the event loop to poll the selector with a zero
    /// timeout instead of blocking, so the remaining work runs after one
    /// non-blocking reactor round.
    #[inline]
    #[cfg(feature = "work_steal")]
    pub fn has_local_tasks(&self, id: usize) -> bool {
        // SAFETY: this is called on the worker that owns `id`; the borrow is
        // derived for this check only, see `local_queue`
        unsafe { self.local_queue(id) }.has_tasks()
    }

    /// See the `work_steal` variant above.
    #[inline]
    #[cfg(not(feature = "work_steal"))]
    pub fn has_local_tasks(&self, id: usize) -> bool {
        !unsafe { self.local_queues.get_unchecked(id) }.is_empty()
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
        // signal one waiting thread if any; skip the syscall when a wakeup
        // for this worker is already in flight
        if !self.arm_wakeup(thread_id) {
            self.get_selector().wakeup(thread_id);
        }
    }

    /// put the coroutine to global queue so that next time it can be scheduled
    ///
    /// `id` must name a worker (`id < self.workers`); `Builder::spawn`
    /// validates it before calling this. The modulo below is only a defensive
    /// fallback so an out-of-range id can never index out of bounds.
    #[inline]
    pub fn schedule_global_with_id(&self, co: CoroutineImpl, id: usize) {
        debug_assert!(id < self.workers, "worker id {id} out of range");
        let thread_id = id.rem_euclid(self.workers);
        // println!("Scheduling to {thread_id}");
        let global = unsafe { self.global_queues.get_unchecked(thread_id) };
        global.push(co);
        // signal one waiting thread if any; see `schedule_global`
        if !self.arm_wakeup(thread_id) {
            self.get_selector().wakeup(thread_id);
        }
    }

    /// Mark a wakeup for worker `id` as in flight, returning whether one was
    /// already armed.
    ///
    /// When `true` is returned the caller must skip the syscall: the armed
    /// wakeup event, or the owning worker's drain before it blocks, already
    /// covers this push. The worker consumes the flag with
    /// [`Self::disarm_wakeup`].
    #[inline]
    fn arm_wakeup(&self, id: usize) -> bool {
        self.wakeup_armed[id].swap(true, Ordering::AcqRel)
    }

    /// Disarm the in-flight wakeup flag for worker `id`, returning whether one
    /// was armed.
    ///
    /// A worker calls this immediately before it decides to block in the
    /// selector. On `true` it must drain its global queue and poll with a zero
    /// timeout instead of blocking: a producer may have coalesced its push
    /// into a wakeup event that was already consumed, leaving queued work with
    /// no event to trigger the drain. The swap orders the worker against every
    /// producer that skipped its syscall, so a skipped push is guaranteed to
    /// be drained in that round.
    #[inline]
    pub fn disarm_wakeup(&self, id: usize) -> bool {
        self.wakeup_armed[id].swap(false, Ordering::AcqRel)
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
    pub fn add_timer(&self, dur: Duration, co: WaitCo) -> timeout_list::TimeoutHandle<WaitCo> {
        self.timer_thread.add_timer(dur, co)
    }

    #[inline]
    pub fn del_timer(&self, handle: timeout_list::TimeoutHandle<WaitCo>) {
        self.timer_thread.del_timer(handle);
    }

    #[inline]
    pub fn get_selector(&self) -> &Selector {
        self.event_loop.get_selector()
    }
}
