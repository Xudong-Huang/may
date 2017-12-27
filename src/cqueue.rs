use std::panic;
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::sync::atomic::{AtomicUsize, Ordering};
use cancel::Cancel;
use join::JoinHandle;
use may_queue::mpsc_list;
use scoped::spawn_unsafe;
use yield_now::yield_with;
use sync::{AtomicOption, Blocker};
use coroutine_impl::{current_cancel_data, run_coroutine, Coroutine, CoroutineImpl, EventSource};

/// This enumeration is the list of the possible reasons that `poll`
/// could not return Event when called.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum PollError {
    /// This cqueue currently has no event and timeout happens
    /// so data may become available in future
    Timeout,

    /// This cqueue associated select coroutines are all finished
    /// so there will never be any more event received on it unless
    /// subscribe new select coroutines by using `add`
    Finished,
}

/// This enumeration is the list of the possible reasons that an event
/// is generated
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum EventKind {
    /// the select coroutine has successfully generated an event from top half
    /// so we can continue it's bottom half after call the `poll`
    Normal,

    /// indicate a select coroutine is finished
    Done,
}

/// The event that `poll` would return, events are generated when a select coroutine
/// has finished it's top half
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
    // the async coroutine that work on an select
    co: Option<CoroutineImpl>,
}

impl Event {
    /// continue the select coroutine with it's bottom half
    /// when `poll` got a Normal event, should always call it first
    fn continue_bottom(&mut self) {
        self.co.take().map(run_coroutine);
    }
}

/// a handle type for the select coroutine
/// you can only use the `remove` method to manually delete the coroutine
pub struct Selector {
    co: Coroutine,
}

impl Selector {
    /// terminate the select coroutine
    /// this would remove the selector from the associated cqueue
    pub fn remove(self) {
        unsafe { self.co.cancel() };
    }
}

/// each select coroutine would use this struct to communicate with
/// the cqueue. the struct is created in `add` for each select coroutine
pub struct EventSender<'a> {
    // index of the select coroutine
    id: usize,
    // associated token, passed from `add`
    token: usize,
    // the select coroutine can use it to pass extra data to the caller
    extra: usize,
    // the mpsc event queue to collect the events
    cqueue: &'a Cqueue,
}

impl<'a> EventSender<'a> {
    /// get the token
    pub fn get_token(&self) -> usize {
        self.token
    }

    /// send out the event
    pub fn send(&self, extra: usize) {
        let cancel = current_cancel_data();
        cancel.check_cancel();
        let me = unsafe { &mut *(self as *const _ as *mut Self) };
        me.extra = extra;
        yield_with(self);
    }
}

impl<'a> EventSource for EventSender<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        self.cqueue.ev_queue.push(Event {
            id: self.id,
            token: self.token,
            extra: self.extra,
            kind: EventKind::Normal,
            co: Some(co),
        });
        self.cqueue
            .to_wake
            .take(Ordering::Acquire)
            .map(|w| w.unpark());
    }

    fn yield_back(&self, _cancel: &'static Cancel) {
        // ignore the cancel to let the bottom half get processed
    }
}

impl<'a> Drop for EventSender<'a> {
    // when the select coroutine finished will trigger this drop
    fn drop(&mut self) {
        self.cqueue.ev_queue.push(Event {
            id: self.id,
            token: self.token,
            extra: self.extra,
            kind: EventKind::Done,
            co: None,
        });
        self.cqueue.cnt.fetch_sub(1, Ordering::Relaxed);
        self.cqueue
            .to_wake
            .take(Ordering::Acquire)
            .map(|w| w.unpark());
    }
}

/// cqueue interface for general select model
pub struct Cqueue {
    // the mpsc queue that transfer event
    ev_queue: mpsc_list::Queue<Event>,
    // thread/coroutine for wake up
    to_wake: AtomicOption<Arc<Blocker>>,
    // track how many coroutines left
    cnt: AtomicUsize,
    // store the select coroutine handles
    selectors: Vec<Option<JoinHandle<()>>>,
    // total created select coroutines
    total: usize,
    // panic status
    is_panicking: bool,
}

unsafe impl Sync for Cqueue {}

impl Cqueue {
    /// register a select coroutine with the cqueue
    /// should use `cqueue_add` and `cqueue_add_oneshot` macros to
    /// create select coroutines correctly
    pub fn add<'a, F>(&self, token: usize, f: F) -> Selector
    where
        F: FnOnce(EventSender) + Send + 'a,
    {
        let sender = EventSender {
            id: self.total,
            token: token,
            extra: 0,
            cqueue: self,
        };
        let h = unsafe { spawn_unsafe(move || f(sender)) };
        let co = h.coroutine().clone();
        self.cnt.fetch_add(1, Ordering::Relaxed);

        let me = unsafe { &mut *(self as *const _ as *mut Self) };
        me.total += 1;
        me.selectors.push(Some(h));
        Selector { co: co }
    }

    // when the select coroutine is done, check the panic status
    // if it's panicked, re throw the panic data
    fn check_panic(&self, id: usize) {
        if self.is_panicking {
            return;
        }

        use generator::Error;
        let me = unsafe { &mut *(self as *const _ as *mut Self) };
        match me.selectors[id]
            .take()
            .expect("join handler not set")
            .join()
        {
            Ok(_) => {}
            Err(panic) => {
                if let Some(err) = panic.downcast_ref::<Error>() {
                    // ignore the cancel panic
                    if *err == Error::Cancel {
                        return;
                    }
                }
                me.is_panicking = true;
                panic::resume_unwind(panic);
            }
        }
    }

    /// poll an event that is ready to process
    /// when the event is returned the bottom half is already run
    /// the API is "completion" mode
    /// if any panic in select coroutine detected during the poll
    /// it will propagate the panic to the caller
    pub fn poll(&self, timeout: Option<Duration>) -> Result<Event, PollError> {
        macro_rules! run_ev {
            ($ev:ident) => ({
                if $ev.kind == EventKind::Done {
                    self.check_panic($ev.id);
                    continue;
                }
                $ev.continue_bottom();
                return Ok($ev);
            })
        }

        let deadline = timeout.map(|dur| Instant::now() + dur);
        loop {
            match self.ev_queue.pop() {
                Some(mut ev) => run_ev!(ev),
                None if self.cnt.load(Ordering::Relaxed) == 0 => return Err(PollError::Finished),
                _ => {}
            }

            let cur = Blocker::current();
            // register the waiter
            self.to_wake.swap(cur.clone(), Ordering::Release);
            // re-check the queue
            match self.ev_queue.pop() {
                None => {
                    cur.park(timeout).ok();
                }
                Some(mut ev) => {
                    self.to_wake.take(Ordering::Relaxed).map(|w| w.unpark());
                    cur.park(timeout).ok();
                    run_ev!(ev);
                }
            }

            // check the timeout
            match deadline {
                Some(d) if Instant::now() >= d => return Err(PollError::Timeout),
                _ => {}
            }
        }
    }
}

impl Drop for Cqueue {
    // this would cancel all unfinished select coroutines
    // and wait until all of them return back
    fn drop(&mut self) {
        // first cancel all the select coroutines if they are running
        self.selectors
            .iter()
            .map(|j| j.as_ref())
            .fold((), |_, join| match join {
                Some(j) if !j.is_done() => unsafe { j.coroutine().cancel() },
                _ => {}
            });

        // if self.is_panicking {
        //     return;
        // }

        // run the rest event
        loop {
            match self.poll(None) {
                Ok(_) => {}
                Err(_e @ PollError::Finished) => break,
                _ => unreachable!("cqueue drop unreachable"),
            }
        }
        // we are sure that all the coroutines are finished
    }
}

/// Create a new `scope`, for select coroutines.
///
/// Scopes, in particular, support scoped select coroutine spawning.
///
pub fn scope<'a, F, R>(f: F) -> R
where
    F: FnOnce(&Cqueue) -> R + 'a,
{
    let cqueue = Cqueue {
        ev_queue: mpsc_list::Queue::new(),
        to_wake: AtomicOption::none(),
        cnt: AtomicUsize::new(0),
        selectors: Vec::new(),
        total: 0,
        is_panicking: false,
    };
    f(&cqueue)
}
