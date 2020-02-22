use std::io;
use std::sync::atomic::Ordering;
use std::time::Duration;

use super::super::{co_io_result, from_nix_error, IoData};
use crate::coroutine_impl::{co_cancel_data, CoroutineImpl, EventSource};
use crate::io::AsIoData;
use crate::scheduler::get_scheduler;
use crate::yield_now::yield_with;
use nix::unistd::read;

pub struct SocketRead<'a> {
    io_data: &'a IoData,
    buf: &'a mut [u8],
    timeout: Option<Duration>,
}

impl<'a> SocketRead<'a> {
    pub fn new<T: AsIoData>(s: &'a T, buf: &'a mut [u8], timeout: Option<Duration>) -> Self {
        SocketRead {
            io_data: s.as_io_data(),
            buf,
            timeout,
        }
    }

    pub fn done(&mut self) -> io::Result<usize> {
        loop {
            co_io_result()?;

            if !self.io_data.is_read_wait() || self.io_data.is_read_ready() {
                self.io_data.reset_read();
                self.io_data.clear_read_wait();

                // finish the read operation
                match read(self.io_data.fd, self.buf) {
                    Ok(n) => return Ok(n),
                    #[cold]
                    Err(e) => {
                        if e == nix::Error::Sys(nix::errno::Errno::EAGAIN) {
                            self.io_data.set_read_wait();
                        } else {
                            return Err(from_nix_error(e));
                        }
                    }
                }
            }

            if (self.io_data.io_flag.fetch_and(!1, Ordering::Relaxed) & 1) != 0 {
                continue;
            }

            // the result is still WouldBlock, need to try again
            yield_with(self);
        }
    }
}

impl<'a> EventSource for SocketRead<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let cancel = co_cancel_data(&co);

        if let Some(dur) = self.timeout {
            get_scheduler()
                .get_selector()
                .add_io_timer(self.io_data, dur);
        }

        // after register the coroutine, it's possible that other thread run it immediately
        // and cause the process after it invalid, this is kind of user and kernel competition
        // so we need to delay the drop of the EventSource, that's why _g is here
        self.io_data.co.swap(co, Ordering::Release);
        // till here the io may be done in other thread

        // there is event, re-run the coroutine
        if self.io_data.is_read_ready() {
            return self.io_data.schedule();
        }

        // register the cancel io data
        cancel.set_io((*self.io_data).clone());
        // re-check the cancel status
        if cancel.is_canceled() {
            unsafe { cancel.cancel() };
        }
    }
}
