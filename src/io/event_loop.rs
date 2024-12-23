use std::io;

use super::sys::{Selector, SysEvent};
use crate::scheduler::{get_scheduler, WORKER_ID};

const IO_POLLS_MAX: usize = 1024;

/// Single threaded IO event loop.
pub struct EventLoop {
    selector: Selector,
}

impl EventLoop {
    pub fn new(io_workers: usize) -> io::Result<EventLoop> {
        Selector::new(io_workers).map(|selector| EventLoop { selector })
    }

    /// Keep spinning the event loop indefinitely, and notify the handler whenever
    /// any of the registered handles are ready.
    pub fn run(&self, id: usize) {
        WORKER_ID.set(id);

        let mut events_buf: [SysEvent; IO_POLLS_MAX] = unsafe { std::mem::zeroed() };
        let mut next_expire = None;
        let selector = &self.selector;
        let scheduler = get_scheduler();

        #[cfg(feature = "io_timeout")]
        let timeout_ns = crate::config().get_timeout_ns();
        #[cfg(not(feature = "io_timeout"))]
        let timeout_ns = 1_000_000_000; // 1s

        loop {
            next_expire = match selector.select(scheduler, id, &mut events_buf, next_expire) {
                Ok(t) => t.or(Some(timeout_ns)),
                Err(e) => panic!("select error = {:?}", e),
            }
        }
    }

    // get the internal selector
    #[inline]
    pub fn get_selector(&self) -> &Selector {
        &self.selector
    }
}
