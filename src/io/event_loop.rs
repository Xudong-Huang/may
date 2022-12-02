use std::io;
use std::sync::atomic::Ordering;

use super::sys::{Selector, SysEvent};
use crate::scheduler::{get_scheduler, WORKER_ID};

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
        use std::mem::MaybeUninit;
        #[cfg(nightly)]
        WORKER_ID.store(id, Ordering::Relaxed);
        #[cfg(not(nightly))]
        WORKER_ID.with(|worker_id| worker_id.store(id, Ordering::Relaxed));

        let events_buf: MaybeUninit<[SysEvent; 1024]> = MaybeUninit::uninit();
        let mut events_buf = unsafe { events_buf.assume_init() };
        let mut co_vec = Vec::with_capacity(events_buf.len());
        // wake up every 1 second
        let mut next_expire = Some(1_000_000_000);

        let scheduler = get_scheduler();
        loop {
            next_expire =
                match self
                    .selector
                    .select(scheduler, id, &mut events_buf, &mut co_vec, next_expire)
                {
                    Ok(v) => v.or(Some(1_000_000_000)),
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
