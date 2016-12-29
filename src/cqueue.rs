use std::panic;
use std::time::{Instant, Duration};
use std::sync::atomic::{AtomicUsize, Ordering};
use queue::mpsc_list;
use join::JoinHandle;
use scoped::spawn_unsafe;
use yield_now::raw_yield_with;
use sync::{AtomicOption, Blocker};
use coroutine::{Coroutine, CoroutineImpl, EventSource, run_coroutine, get_cancel_data};

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
    /// the tocket associated with the select coroutine
    pub tocken: usize,
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
    // associated tocken, passed from `add`
    tocken: usize,
    // the select coroutine can use it to pass extra data to the caller
    extra: usize,
    // the mpsc event queue to collect the events
    cqueue: &'a Cqueue,
}

impl<'a> EventSender<'a> {
    /// get the tocken
    pub fn get_tocken(&self) -> usize {
        self.tocken
    }

    /// send out the event
    pub fn send(&self, extra: usize) {
        let cancel = get_cancel_data();
        if cancel.is_canceled() {
            use generator::Error;
            panic!(Error::Cancel);
        }

        let me = unsafe { &mut *(self as *const _ as *mut Self) };
        me.extra = extra;
        // don't process the panic
        // just let the bottom half run
        raw_yield_with(self);
    }
}

impl<'a> EventSource for EventSender<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        self.cqueue.ev_queue.push(Event {
            id: self.id,
            tocken: self.tocken,
            extra: self.extra,
            kind: EventKind::Normal,
            co: Some(co),
        });
        self.cqueue.to_wake.take(Ordering::Acquire).map(|w| w.unpark());
    }
}

impl<'a> Drop for EventSender<'a> {
    // when the select coroutine finished will trigger this drop
    fn drop(&mut self) {
        self.cqueue.ev_queue.push(Event {
            id: self.id,
            tocken: self.tocken,
            extra: self.extra,
            kind: EventKind::Done,
            co: None,
        });
        self.cqueue.cnt.fetch_sub(1, Ordering::Relaxed);
        self.cqueue.to_wake.take(Ordering::Acquire).map(|w| w.unpark());
    }
}

/// cqueue interface for general select model
pub struct Cqueue {
    // the mpsc queue that transfer event
    ev_queue: mpsc_list::Queue<Event>,
    // thread/coroutine for wake up
    to_wake: AtomicOption<Blocker>,
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
    /// create select coroutines correclty
    pub fn add<'a, F>(&self, tocken: usize, f: F) -> Selector
        where F: FnOnce(EventSender) + Send + 'a
    {
        let sender = EventSender {
            id: self.total,
            tocken: tocken,
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
    // if it's paniced, re throw the panic data
    fn check_panic(&self, id: usize) {
        if self.is_panicking {
            return;
        }

        use generator::Error;
        let me = unsafe { &mut *(self as *const _ as *mut Self) };
        match me.selectors[id].take().expect("join handler not set").join() {
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

            // register the waiter
            self.to_wake.swap(Blocker::current(), Ordering::Release);
            // re-check the queue
            match self.ev_queue.pop() {
                None => Blocker::park(timeout),
                Some(mut ev) => {
                    self.to_wake.take(Ordering::Relaxed).map(|w| w.unpark());
                    Blocker::park(timeout);
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
        self.selectors.iter().map(|j| j.as_ref()).fold((), |_, join| {
            match join {
                Some(j) if !j.is_done() => unsafe { j.coroutine().cancel() },
                _ => {}
            }
        });

        if self.is_panicking {
            return;
        }

        // run the rest event
        loop {
            match self.poll(None) {
                Ok(_) => {}
                Err(_e @ PollError::Finished) => break,
                _ => unreachable!("cqueue drop unreachable"),
            }
        }
        // we are sure that all the coroutines are finised
    }
}

/// Create a new `scope`, for select coroutines.
///
/// Scopes, in particular, support scoped select coroutine spawning.
///
pub fn scope<'a, F, R>(f: F) -> R
    where F: FnOnce(&Cqueue) -> R + 'a
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

/// macro used to create the select coroutine
/// that will run in a infinite loop, and generate
/// as many events as possible
#[macro_export]
macro_rules! cqueue_add{
    (
        $cqueue:ident, $tocken:expr, $name:pat = $top:expr => $bottom:expr
    ) => ({
        $cqueue.add($tocken, |es| {
            loop {
                let $name = $top;
                es.send(es.get_tocken());
                $bottom
            }
        })
    })
}


/// macro used to create the select coroutine
/// that will run only once, thus generate only one event
#[macro_export]
macro_rules! cqueue_add_oneshot{
    (
        $cqueue:ident, $tocken:expr, $name:pat = $top:expr => $bottom:expr
    ) => ({
        $cqueue.add($tocken, |es| {
            let $name = $top;
            es.send(es.get_tocken());
            $bottom
        })
    })
}

/// macro used to select for only one event
/// it will return the index of which event happens first
#[macro_export]
macro_rules! select {
    (
        $($name:pat = $top:expr => $bottom:expr),+
    ) => ({
        use $crate::cqueue;
        cqueue::scope(|cqueue| {
            let mut _tocken = 0;
            $(
                cqueue_add_oneshot!(cqueue, _tocken, $name = $top => $bottom);
                _tocken += 1;
            )+
            match cqueue.poll(None) {
                Ok(ev) => return ev.tocken,
                _ => unreachable!("select error"),
            }
        })
    })
}
