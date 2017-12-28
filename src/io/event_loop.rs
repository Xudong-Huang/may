use std::io;
use super::sys::{Selector, SysEvent};
use scheduler::get_scheduler;
use coroutine_impl::{run_coroutine, CoroutineImpl};

/// Single threaded IO event loop.
pub struct EventLoop {
    selector: Selector,
}

fn schedule_io_default(co: CoroutineImpl) {
    run_coroutine(co);
}

fn schedule_io_on_worker(co: CoroutineImpl) {
    println!("on my dear");
    get_scheduler().schedule(co);
}

impl EventLoop {
    pub fn new(io_workers: usize, run_on_io: bool) -> io::Result<EventLoop> {
        let schedule_policy = if run_on_io {
            schedule_io_default
        } else {
            schedule_io_on_worker
        };

        Selector::new(io_workers, schedule_policy).map(|selector| EventLoop { selector: selector })
    }

    /// Keep spinning the event loop indefinitely, and notify the handler whenever
    /// any of the registered handles are ready.
    pub fn run(&self, id: usize) -> io::Result<()> {
        let mut events_buf: [SysEvent; 1024] = unsafe { ::std::mem::uninitialized() };
        let mut next_expire = None;
        loop {
            next_expire = match self.selector.select(id, &mut events_buf, next_expire) {
                Ok(v) => v,
                Err(e) => {
                    println!("selector error={:?}", e);
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
