use std::io;
use std::sync::atomic::Ordering;

use super::sys::{Selector, SysEvent};
use crate::scheduler::WORKER_ID;

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
    pub fn run(&self, id: usize) -> io::Result<()> {
        #[cfg(nightly)]
        WORKER_ID.store(id, Ordering::Relaxed);
        #[cfg(not(nightly))]
        WORKER_ID.with(|worker_id| worker_id.store(id, Ordering::Relaxed));

        let mut events_buf: [SysEvent; 1024] =
            unsafe { std::mem::MaybeUninit::uninit().assume_init() };
        // wake up every 1 second
        let mut next_expire = Some(1_000_000_000);
        loop {
            next_expire = match self.selector.select(id, &mut events_buf, next_expire) {
                Ok(v) => v.or(Some(1_000_000_000)),
                #[cold]
                Err(e) => {
                    error!("selector error={:?}", e);
                    continue;
                }
            }
        }
    }

    // get the internal selector
    #[inline]
    pub fn get_selector(&self) -> &Selector {
        &self.selector
    }
}
