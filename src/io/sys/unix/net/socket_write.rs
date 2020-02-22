use std::io;
use std::sync::atomic::Ordering;
use std::time::Duration;

use super::super::{co_io_result, from_nix_error, IoData};
use crate::coroutine_impl::{CoroutineImpl, EventSource};
use crate::io::AsIoData;
use crate::scheduler::get_scheduler;
use crate::yield_now::yield_with;
use nix::unistd::write;

pub struct SocketWrite<'a> {
    io_data: &'a IoData,
    buf: &'a [u8],
    timeout: Option<Duration>,
}

impl<'a> SocketWrite<'a> {
    pub fn new<T: AsIoData>(s: &'a T, buf: &'a [u8], timeout: Option<Duration>) -> Self {
        SocketWrite {
            io_data: s.as_io_data(),
            buf,
            timeout,
        }
    }

    pub fn done(&mut self) -> io::Result<usize> {
        loop {
            co_io_result()?;

            if !self.io_data.is_write_wait() || self.io_data.is_write_ready() {
                self.io_data.reset_write();
                self.io_data.clear_write_wait();

                match write(self.io_data.fd, self.buf) {
                    Ok(n) => return Ok(n),
                    #[cold]
                    Err(e) => {
                        if e == nix::Error::Sys(nix::errno::Errno::EAGAIN) {
                            self.io_data.set_write_wait();
                        } else {
                            return Err(from_nix_error(e));
                        }
                    }
                }
            }

            if (self.io_data.io_flag.fetch_and(!2, Ordering::Relaxed) & 2) != 0 {
                continue;
            }

            // the result is still WouldBlock, need to try again
            yield_with(self);
        }
    }
}

impl<'a> EventSource for SocketWrite<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        if let Some(dur) = self.timeout {
            get_scheduler()
                .get_selector()
                .add_io_timer(self.io_data, dur);
        }
        self.io_data.co.swap(co, Ordering::Release);

        // there is event, re-run the coroutine
        if self.io_data.is_write_ready() {
            self.io_data.schedule();
        }
    }
}
