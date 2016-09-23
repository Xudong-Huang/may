use std::io;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use super::sys::{Selector, EventData, TimerData, Events, timeout_handler};
use coroutine::CoroutineImpl;
use timeout_list::{TimeOutList, now};
use queue::spmc::Queue as spmc;

type TimerList = TimeOutList<TimerData>;

/// Single threaded IO event loop.
pub struct EventLoop {
    selector: Selector,
    timer_list: TimerList,
}

impl EventLoop {
    pub fn new(workers: usize) -> io::Result<EventLoop> {
        let selector = try!(Selector::new(workers));
        Ok(EventLoop {
            selector: selector,
            timer_list: TimerList::new(),
        })
    }

    /// Keep spinning the event loop indefinitely, and notify the handler whenever
    /// any of the registered handles are ready.
    pub fn run(&mut self) -> io::Result<()> {
        let mut events = Events::new();
        let mut next_expire = None;
        loop {
            // first run the selector
            try!(self.selector.select(&mut events, next_expire));
            // deal with the timer list
            let now = now();
            next_expire = self.timer_list.schedule_timer(now, &timeout_handler);
        }
    }

    // register the io request to both selector and the timeout list
    pub fn add_io(&self, io: &mut EventData, timeout: Option<Duration>) -> io::Result<()> {
        let timer_handle = timeout.map(|dur| self.timer_list.add_timer(dur, io.timer_data()));
        try!(self.selector.add_io(io));
        // set the io timer handle
        io.timer = timer_handle;
        Ok(())
    }

    // used by the scheduler to pull the coroutine event list
    #[inline]
    pub fn get_event_list(&self) -> &spmc<CoroutineImpl> {
        self.selector.get_event_list()
    }
}
