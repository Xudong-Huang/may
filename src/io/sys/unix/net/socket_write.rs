use std::io;
use std::sync::atomic::Ordering;
use std::time::Duration;

use super::super::{co_io_result, from_nix_error, IoData};
use crate::coroutine_impl::{CoroutineImpl, EventSource};
use crate::io::AsIoData;
use crate::scheduler::get_scheduler;
use crate::sync::delay_drop::DelayDrop;
use crate::yield_now::yield_with;
use nix::unistd::write;

pub struct SocketWrite<'a> {
    io_data: &'a IoData,
    buf: &'a [u8],
    timeout: Option<Duration>,
    can_drop: DelayDrop,
}

impl<'a> SocketWrite<'a> {
    pub fn new<T: AsIoData>(s: &'a T, buf: &'a [u8], timeout: Option<Duration>) -> Self {
        SocketWrite {
            io_data: s.as_io_data(),
            buf,
            timeout,
            can_drop: DelayDrop::new(),
        }
    }

    pub fn done(&mut self) -> io::Result<usize> {
        loop {
            co_io_result()?;

            // clear the io_flag
            self.io_data.io_flag.store(false, Ordering::Relaxed);

            match write(self.io_data.fd, self.buf) {
                Ok(n) => return Ok(n),
                #[cold]
                Err(e) => {
                    if e == nix::Error::Sys(nix::errno::Errno::EAGAIN) {
                        // do nothing
                    } else {
                        return Err(from_nix_error(e));
                    }
                }
            }

            if self.io_data.io_flag.swap(false, Ordering::Relaxed) {
                continue;
            }

            // the result is still WouldBlock, need to try again
            self.can_drop.reset();
            yield_with(self);
        }
    }
}

impl<'a> EventSource for SocketWrite<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let _g = self.can_drop.delay_drop();
        if let Some(dur) = self.timeout {
            get_scheduler()
                .get_selector()
                .add_io_timer(self.io_data, dur);
        }
        self.io_data.co.swap(co, Ordering::Release);

        // there is event, re-run the coroutine
        if self.io_data.io_flag.load(Ordering::Acquire) {
            self.io_data.schedule();
        }
    }
}
