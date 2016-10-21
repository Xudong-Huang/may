use std::io;
use super::sys::{Selector, SysEvent};

/// Single threaded IO event loop.
pub struct EventLoop {
    selector: Selector,
}

impl EventLoop {
    pub fn new(io_workers: usize) -> io::Result<EventLoop> {
        Selector::new(io_workers).map(|selector| EventLoop { selector: selector })
    }

    /// Keep spinning the event loop indefinitely, and notify the handler whenever
    /// any of the registered handles are ready.
    pub fn run(&self, id: usize) -> io::Result<()> {
        let mut events_buf: [SysEvent; 1024] = unsafe { ::std::mem::uninitialized() };
        let mut next_expire = None;
        loop {
            next_expire = try!(self.selector.select(id, &mut events_buf, next_expire));
        }
    }

    // get the internal selector
    #[inline]
    pub fn get_selector(&self) -> &Selector {
        &self.selector
    }
}
