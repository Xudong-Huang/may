use std::io;
use std::time::Duration;
use scheduler::get_scheduler;
use timeout_list::{TimeOutList, now};
use super::sys::{Selector, EventData, TimerData, EventsBuf, timeout_handler};

type TimerList = TimeOutList<TimerData>;

/// Single threaded IO event loop.
pub struct EventLoop {
    selector: Selector,
    timer_list: TimerList,
}

impl EventLoop {
    pub fn new() -> io::Result<EventLoop> {
        let selector = try!(Selector::new());
        Ok(EventLoop {
            selector: selector,
            timer_list: TimerList::new(),
        })
    }

    /// Keep spinning the event loop indefinitely, and notify the handler whenever
    /// any of the registered handles are ready.
    pub fn run(&self) -> io::Result<()> {
        let s = get_scheduler();
        let mut events = EventsBuf::new();
        let len = events.capacity();
        unsafe { events.set_len(len) };
        let mut next_expire = None;
        loop {
            // first run the selector
            try!(self.selector.select(s, &mut events, next_expire));
            // deal with the timer list
            let now = now();
            next_expire = self.timer_list.schedule_timer(now, &timeout_handler);
        }
    }

    // get the internal selector
    #[inline]
    pub fn get_selector(&self) -> &Selector {
        &self.selector
    }

    // register the io request to both selector and the timeout list
    pub fn add_io(&self, io: &mut EventData, timeout: Option<Duration>) -> io::Result<()> {
        println!("timeout = {:?}", timeout);
        let timer_handle = timeout.map(|dur| self.timer_list.add_timer(dur, io.timer_data()));
        let ret = self.selector.add_io(io);
        if ret.is_ok() {
            io.timer = timer_handle;
        } else {
            timer_handle.map(|t| t.remove());
        }

        ret
    }
}
