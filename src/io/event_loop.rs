use std::io;
use std::time::Duration;
use scheduler::get_scheduler;
use timeout_list::{TimeOutList, now};
use super::sys::{Selector, SysEvent, EventData, TimerData, timeout_handler};

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
        let mut events_buf: [SysEvent; 1024] = unsafe { ::std::mem::uninitialized() };
        let mut next_expire = None;
        loop {
            // first run the selector
            try!(self.selector.select(s, &mut events_buf, next_expire));
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
    pub fn add_io_timer(&self, io: &mut EventData, timeout: Option<Duration>) {
        io.timer = timeout.map(|dur| {
            info!("io timeout = {:?}", dur);
            let (h, b_new) = self.timer_list.add_timer(dur, io.timer_data());
            if b_new {
                // wakeup the event loop threead to recal the next wait
                self.selector.wakeup();
            }
            h
        });
    }
}
