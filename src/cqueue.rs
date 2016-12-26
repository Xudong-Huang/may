use std::time::{Instant, Duration};
use std::sync::atomic::{AtomicUsize, Ordering};
use queue::mpsc_list;
use join::JoinHandle;
use scoped::spawn_unsafe;
use yield_now::yield_with;
use sync::{AtomicOption, Blocker};
use coroutine::{Coroutine, CoroutineImpl, EventSource, run_coroutine};

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
pub enum EventKind {
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
    /// the event type
    pub kind: EventKind,
    /// the select coroutine can use it to pass extra data with the caller
    pub extra: usize,

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
    // associated tocken
    tocken: usize,
    /// the select coroutine can use it to pass extra data with the caller
    pub extra: usize,
    // the mpsc event queue to collect the events
    cqueue: &'a Cqueue,
}

impl<'a> EventSender<'a> {
    /// send out the event
    pub fn send(&self) {
        yield_with(self);
    }
}

impl<'a> EventSource for EventSender<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        self.cqueue.ev_queue.push(Event {
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
    selectors: mpsc_list::Queue<JoinHandle<()>>,
}

impl Cqueue {
    // cqueue can't live on stack because of the drop would
    // run in a new stack location and we need drop to poll
    // out all the rest events
    pub fn new() -> Box<Self> {
        Box::new(Cqueue {
            ev_queue: mpsc_list::Queue::new(),
            to_wake: AtomicOption::none(),
            cnt: AtomicUsize::new(0),
            selectors: mpsc_list::Queue::new(),
        })
    }

    /// register a select coroutine with the cqueue
    /// should use `cqueue_add` and `cqueue_add_oneshot` macros to
    /// create select coroutines correclty
    pub fn add<'a, F>(&self, tocken: usize, f: F) -> Selector
        where F: FnOnce(EventSender) + Send + 'a
    {
        let sender = EventSender {
            tocken: tocken,
            extra: 0,
            cqueue: self,
        };
        let h = unsafe { spawn_unsafe(move || f(sender)) };
        let co = h.coroutine().clone();
        self.cnt.fetch_add(1, Ordering::Relaxed);
        self.selectors.push(h);
        Selector { co: co }
    }

    /// poll an event that is ready to process
    /// when the event is returned the bottom half is already run
    /// the API is "completion" mode
    pub fn poll(&self, timeout: Option<Duration>) -> Result<Event, PollError> {
        let deadline = timeout.map(|dur| Instant::now() + dur);
        loop {
            match self.ev_queue.pop() {
                Some(mut ev) => {
                    ev.continue_bottom();
                    return Ok(ev);
                }
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
                    ev.continue_bottom();
                    return Ok(ev);
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
        while let Some(join) = self.selectors.pop() {
            if !join.is_done() {
                let co = join.coroutine();
                unsafe { co.cancel() };
            }
        }
        // run the rest event
        loop {
            match self.poll(Some(Duration::from_millis(100))) {
                Ok(_) => {}
                Err(_e @ PollError::Finished) => break,
                _ => unreachable!("cqueue drop unreachable"),
            }
        }
        // we are sure that all the coroutines are finised
    }
}

/// macro used to create the select coroutine
/// that will run in a infinite loop, and generate
/// as many events as possible
#[macro_export]
macro_rules! cqueue_add{
    (
        $cqueue:ident, $tocken:expr, {$top:stmt}, {$bottom:stmt}
    ) => ({
        $cqueue.add($tocken, |es| {
            loop {
                $top;
                yield_with(&es);
                $bottom;
            }
        })
    })
}

/// macro used to create the select coroutine
/// that will run only once, thus generate only one event
#[macro_export]
macro_rules! cqueue_add_oneshot{
    (
        $cqueue:ident, $tocken:expr, {$top:stmt}, {$bottom:stmt}
    ) => ({
        $cqueue.add($tocken, |es| {
            $top;
            $crate::cqueue::yield_with(&es);
            $bottom;
        })
    })
}

#[macro_export]
macro_rules! select {
    (
        $($name:pat = $rx:ident.$meth:ident() => $code:expr),+
    ) => ({
        use $crate::sync::mpsc::Select;
        let sel = Select::new();
        $( let mut $rx = sel.handle(&$rx); )+
        unsafe {
            $( $rx.add(); )+
        }
        let ret = sel.wait();
        $( if ret == $rx.id() { let $name = $rx.$meth(); $code } else )+
        { unreachable!() }
    })
}
