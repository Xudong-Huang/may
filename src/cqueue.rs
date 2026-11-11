//! A select-style event queue for scoped coroutines.
//!
//! [`scope`] creates a [`Cqueue`]. Each selector added with [`Cqueue::add`]
//! (usually through the `cqueue_add!` / `cqueue_add_oneshot!` macros) runs as
//! a coroutine: its *top half* runs until it calls [`EventSender::send`], which
//! queues an event and suspends the selector. [`Cqueue::poll`] later resumes
//! that selector's *bottom half* on the polling coroutine, so bottom halves
//! run sequentially and only on the poller.
//!
//! # Constraints
//!
//! * Only one coroutine/thread may call [`Cqueue::poll`] on a given cqueue: the
//!   event queue has a single consumer and `to_wake` holds a single waiter.
//! * Selectors must all be registered before polling starts, and the cqueue
//!   has a single owner: [`Cqueue::add`] must not run concurrently with
//!   another `add` or with `poll`. Debug builds assert this.
//! * An [`EventSender`] must only be used from the selector coroutine it was
//!   passed to. It is `Send` because the selector closure captures it, but
//!   `send` yields the *current* coroutine, so using it from another coroutine
//!   would resume the wrong one.
//! * A panic in a selector is re-raised by a later [`Cqueue::poll`], or by
//!   `Drop` after every selector has been cancelled and drained; only the
//!   first selector panic is propagated. If the thread is already unwinding,
//!   the panic is dropped instead of aborting the process.

use std::any::Any;
use std::panic;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::cancel::Cancel;
use crate::coroutine_impl::{
    current_cancel_data, run_coroutine, Coroutine, CoroutineImpl, EventSource,
};
use crate::join::JoinHandle;
use crate::scoped::spawn_unsafe;
use crate::sync::Mutex;
use crate::sync::{AtomicOption, Blocker};
use crate::yield_now::yield_with;

use generator::Error;
use may_queue::mpsc::Queue;

/// The list of possible reasons that `poll` could not return an event.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum PollError {
    /// The cqueue has no ready event and the timeout expired; an event may
    /// become available in the future.
    Timeout,

    /// All select coroutines associated with this cqueue have finished, so no
    /// more events can be received unless new select coroutines are added with
    /// [`Cqueue::add`].
    Finished,
}

/// This enumeration is the list of the possible reasons that an event
/// is generated
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum EventKind {
    /// the select coroutine has successfully generated an event from its top
    /// half, so its bottom half can be continued by `poll`
    Normal,

    /// indicates a select coroutine is finished
    Done,
}

/// The event that `poll` returns. Events are generated when a select coroutine
/// reaches [`EventSender::send`] in its top half.
///
/// Returned events never own the coroutine: the bottom half has already run by
/// the time `poll` returns it.
#[derive(Debug)]
pub struct Event {
    /// the token associated with the select coroutine
    pub token: usize,
    /// the select coroutine can use it to pass extra data with the caller
    pub extra: usize,
    /// id of the select coroutine, used internally to locate the JoinHandle
    id: usize,
    /// the event type
    kind: EventKind,
    // the suspended coroutine that will run the bottom half, if any
    co: Option<CoroutineImpl>,
}

impl Event {
    /// continue the select coroutine with its bottom half
    /// when `poll` got a Normal event, should always call it first
    fn continue_bottom(&mut self) {
        if let Some(co) = self.co.take() {
            run_coroutine(co);
        }
    }
}

/// A handle to a select coroutine.
///
/// Dropping the handle does not stop the coroutine: use [`Selector::remove`]
/// to cancel it. Cancellation is asynchronous, so events already queued may
/// still be delivered before the coroutine finishes.
pub struct Selector {
    co: Coroutine,
}

impl Selector {
    /// Terminate the select coroutine.
    ///
    /// This cancels the coroutine; it is removed from the cqueue
    /// asynchronously, once it reaches a cancellation point.
    pub fn remove(self) {
        self.co.cancel();
    }
}

/// Per-selector handle used to communicate with the cqueue.
///
/// Created by [`Cqueue::add`] and passed to the selector closure. It must only
/// be used by that selector coroutine.
pub struct EventSender<'a> {
    // index of the select coroutine
    id: usize,
    // associated token, passed from `add`
    token: usize,
    // the select coroutine can use it to pass extra data to the caller
    extra: AtomicUsize,
    // the mpsc event queue to collect the events
    cqueue: &'a Cqueue,
}

unsafe impl Send for EventSender<'_> {}

impl EventSender<'_> {
    /// Get the token.
    pub fn get_token(&self) -> usize {
        self.token
    }

    /// Publish an event and suspend until `poll` has run the bottom half.
    ///
    /// # Panics
    ///
    /// Panics if called outside a coroutine context.
    pub fn send(&self, extra: usize) {
        let cancel = current_cancel_data();
        cancel.check_cancel();
        self.extra.store(extra, Ordering::Relaxed);
        yield_with(self);
    }
}

impl EventSource for EventSender<'_> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        // Publish before waking: the poller re-checks the queue after
        // registering its blocker, so either order is observed.
        self.cqueue.ev_queue.push(Event {
            id: self.id,
            token: self.token,
            extra: self.extra.load(Ordering::Relaxed),
            kind: EventKind::Normal,
            co: Some(co),
        });
        if let Some(w) = self.cqueue.to_wake.take() {
            w.unpark();
        }
    }

    fn yield_back(&self, _cancel: &'static Cancel) {
        // ignore the cancel to let the bottom half get processed
    }
}

impl Drop for EventSender<'_> {
    // when the select coroutine finishes this drop is triggered
    fn drop(&mut self) {
        self.cqueue.ev_queue.push(Event {
            id: self.id,
            token: self.token,
            extra: self.extra.load(Ordering::Relaxed),
            kind: EventKind::Done,
            co: None,
        });
        // Publish the done event before decrementing `cnt`: the polling side
        // reads `cnt` with acquire ordering and re-checks the queue, so it
        // cannot report `Finished` while this event is still invisible.
        self.cqueue.cnt.fetch_sub(1, Ordering::Release);
        if let Some(w) = self.cqueue.to_wake.take() {
            w.unpark();
        }
    }
}

/// Debug-only guard asserting that a cqueue has a single owner: `add` and
/// `poll` must not run concurrently.
#[cfg(debug_assertions)]
struct BusyGuard<'a>(&'a AtomicBool);

#[cfg(debug_assertions)]
impl<'a> BusyGuard<'a> {
    fn acquire(flag: &'a AtomicBool, what: &str) -> Self {
        if flag.swap(true, Ordering::SeqCst) {
            panic!("Cqueue::{what} called concurrently with add/poll, which is not supported");
        }
        BusyGuard(flag)
    }
}

#[cfg(debug_assertions)]
impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// The event queue for the select model.
///
/// Created by [`scope`]; a single coroutine/thread polls it while the select
/// coroutines publish events.
pub struct Cqueue {
    // the mpsc queue that transfers events
    ev_queue: Queue<Event>,
    // thread/coroutine for wake up
    to_wake: AtomicOption<Arc<Blocker>>,
    // track how many coroutines are left
    cnt: AtomicUsize,
    // store the select coroutine handles
    selectors: Mutex<Vec<Option<JoinHandle<()>>>>,
    // total created select coroutines
    total: AtomicUsize,
    // set once a selector panic has escaped through `poll`; later panics are
    // not re-raised
    is_panicking: AtomicBool,
    // set while `drop` cancels and drains the selectors; a selector panic is
    // recorded in `panic_data` instead of unwinding in the middle of the drain
    draining: AtomicBool,
    // the first selector panic recorded during the drain
    panic_data: AtomicOption<Box<dyn Any + Send>>,
    // debug-only guard that `add`/`poll` are not used concurrently
    #[cfg(debug_assertions)]
    busy: AtomicBool,
}

impl Cqueue {
    /// Register a select coroutine with the cqueue.
    ///
    /// The `cqueue_add!` and `cqueue_add_oneshot!` macros should be used to
    /// create select coroutines correctly. All `add` calls must be sequential
    /// and must not overlap with [`Cqueue::poll`]: debug builds assert this.
    pub fn add<'a, F>(&self, token: usize, f: F) -> Selector
    where
        F: FnOnce(EventSender) + Send + 'a,
    {
        #[cfg(debug_assertions)]
        let _busy = BusyGuard::acquire(&self.busy, "add");

        let sender = EventSender {
            id: self.total.load(Ordering::Relaxed),
            token,
            extra: 0.into(),
            cqueue: self,
        };
        let h = unsafe { spawn_unsafe(move || f(sender)) };
        let co = h.coroutine().clone();
        self.cnt.fetch_add(1, Ordering::Release);

        self.total.fetch_add(1, Ordering::Relaxed);
        self.selectors.lock().unwrap().push(Some(h));
        Selector { co }
    }

    // when the select coroutine is done, check the panic status
    // if it's panicked, re throw the panic data
    fn check_panic(&self, id: usize) {
        if self.is_panicking.load(Ordering::Relaxed) {
            return;
        }

        // Take the handle and release the `selectors` lock before joining:
        // `join` can park, and the `resume_unwind` below must not run while
        // the lock is held, otherwise the unwind poisons the mutex and a later
        // `lock().unwrap()` (e.g. in `Drop` while unwinding) aborts.
        let handle = {
            let mut selectors = self.selectors.lock().unwrap();
            selectors.get_mut(id).and_then(|slot| slot.take())
        };

        match handle.expect("join handler not set").join() {
            Ok(_) => {}
            Err(panic) => {
                if let Some(err) = panic.downcast_ref::<Error>() {
                    // ignore the cancel panic
                    if *err == Error::Cancel {
                        return;
                    }
                }
                if self.draining.load(Ordering::Relaxed) {
                    // `Drop` must finish cancelling and draining every
                    // selector before a panic may escape, otherwise the
                    // remaining selectors would outlive this cqueue. Keep the
                    // first panic and drop any later ones. Only the polling
                    // thread runs the drain, so this cannot race.
                    if let Some(first) = self.panic_data.take() {
                        self.panic_data.store(first);
                    } else {
                        self.panic_data.store(panic);
                    }
                    return;
                }
                self.is_panicking.store(true, Ordering::Relaxed);
                panic::resume_unwind(panic);
            }
        }
    }

    /// Poll an event that is ready to process.
    ///
    /// When an event is returned its bottom half has already run on this
    /// coroutine. A panic in a selector is not returned here: it is re-raised
    /// (once) if the selector's `Done` event is processed by this call, or by
    /// a later call to `poll`/`Drop`.
    ///
    /// `timeout` is an overall deadline for the waits, not a per-wait budget;
    /// a ready event is returned even when the deadline has passed.
    ///
    /// Only one coroutine/thread may call `poll` on a given cqueue.
    pub fn poll(&self, timeout: Option<Duration>) -> Result<Event, PollError> {
        #[cfg(debug_assertions)]
        let _busy = BusyGuard::acquire(&self.busy, "poll");

        macro_rules! run_ev {
            ($ev:ident) => {{
                // No waiter is needed once an event is in hand: clear any
                // registration left from this or an earlier iteration.
                self.to_wake.take();
                if $ev.kind == EventKind::Done {
                    self.check_panic($ev.id);
                    continue;
                }
                $ev.continue_bottom();
                return Ok($ev);
            }};
        }

        let deadline = timeout.map(|dur| Instant::now() + dur);
        loop {
            match self.ev_queue.pop() {
                Some(mut ev) => run_ev!(ev),
                None => {
                    if self.cnt.load(Ordering::Acquire) == 0 {
                        // The last selector publishes its final event before
                        // decrementing `cnt` (release), so re-check the queue:
                        // that event may have been published after the pop
                        // above but before the acquire load.
                        match self.ev_queue.pop() {
                            Some(mut ev) => run_ev!(ev),
                            None => {
                                self.to_wake.take();
                                return Err(PollError::Finished);
                            }
                        }
                    }
                }
            }

            // Wait for the remaining part of the timeout only, so the total
            // time spent in `poll` stays bounded by the caller's timeout.
            let remaining = match deadline {
                Some(deadline) => match deadline.checked_duration_since(Instant::now()) {
                    Some(remaining) if !remaining.is_zero() => Some(remaining),
                    _ => {
                        self.to_wake.take();
                        return Err(PollError::Timeout);
                    }
                },
                None => None,
            };

            let cur = Blocker::current();
            // register the waiter
            self.to_wake.store(cur.clone());
            // re-check the queue
            match self.ev_queue.pop() {
                None => {
                    cur.park(remaining).ok();
                }
                Some(mut ev) => run_ev!(ev),
            }
        }
    }
}

impl Drop for Cqueue {
    // this would cancel all unfinished select coroutines
    // and wait until all of them return back
    fn drop(&mut self) {
        // `check_panic` records selector panics while this is set: raising one
        // in the middle of the drain would abandon the remaining selectors
        // while they still reference this cqueue.
        self.draining.store(true, Ordering::Relaxed);

        // first cancel all the select coroutines if they are running
        for join in self.selectors.lock().unwrap().iter().flatten() {
            if !join.is_done() {
                join.coroutine().cancel();
            }
        }

        // run the rest event
        loop {
            match self.poll(None) {
                Ok(_) => {}
                Err(PollError::Finished) => break,
                Err(PollError::Timeout) => unreachable!("cqueue drop cannot time out"),
            }
        }

        // we are sure that all the coroutines are finished now, so it is safe
        // to propagate the first selector panic. If the thread is already
        // unwinding, re-raising would abort the process, so drop it instead.
        let panic = self.panic_data.take();
        if !std::thread::panicking() {
            if let Some(panic) = panic {
                panic::resume_unwind(panic);
            }
        }
    }
}

/// Create a new `scope` for select coroutines.
///
/// Select coroutines spawned inside the scope can borrow from the enclosing
/// stack frame; when the scope ends all of them are cancelled and joined
/// before the borrowed data goes away. A panic from a selector is re-raised
/// after that cleanup.
pub fn scope<'a, F, R>(f: F) -> R
where
    F: FnOnce(&Cqueue) -> R + 'a,
{
    let cqueue = Cqueue {
        ev_queue: Queue::new(),
        to_wake: AtomicOption::none(),
        cnt: AtomicUsize::new(0),
        selectors: Mutex::new(Vec::new()),
        total: AtomicUsize::new(0),
        is_panicking: AtomicBool::new(false),
        draining: AtomicBool::new(false),
        panic_data: AtomicOption::none(),
        #[cfg(debug_assertions)]
        busy: AtomicBool::new(false),
    };
    f(&cqueue)
}
