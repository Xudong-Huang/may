use std::io;
use std::sync::atomic::Ordering;

use super::sys::{Selector, SysEvent};
use crate::coroutine_impl::{run_coroutine, CoroutineImpl};
use crate::scheduler::{get_scheduler, WORKER_ID};

/// Single threaded IO event loop.
pub struct EventLoop {
    selector: Selector,
}

#[inline]
fn schedule_io_default(co: CoroutineImpl) {
    run_coroutine(co);
}

#[inline]
fn schedule_io_on_worker(co: CoroutineImpl) {
    get_scheduler().schedule_global(co);
}

impl EventLoop {
    pub fn new(io_workers: usize, run_on_io: bool) -> io::Result<EventLoop> {
        let schedule_policy = if run_on_io {
            schedule_io_default
        } else {
            schedule_io_on_worker
        };

        Selector::new(io_workers, schedule_policy).map(|selector| EventLoop { selector })
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
        let mut next_expire = None;
        loop {
            next_expire = match self.selector.select(id, &mut events_buf, next_expire) {
                Ok(v) => v,
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
