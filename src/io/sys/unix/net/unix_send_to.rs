use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::{self, io};

use super::super::{co_io_result, IoData};
use crate::coroutine_impl::{CoroutineImpl, EventSource};
use crate::io::AsIoData;
use crate::os::unix::net::UnixDatagram;
use crate::scheduler::get_scheduler;
use crate::yield_now::yield_with;

pub struct UnixSendTo<'a> {
    io_data: &'a IoData,
    buf: &'a [u8],
    socket: &'a std::os::unix::net::UnixDatagram,
    path: &'a Path,
    timeout: Option<Duration>,
}

impl<'a> UnixSendTo<'a> {
    pub fn new(socket: &'a UnixDatagram, buf: &'a [u8], path: &'a Path) -> io::Result<Self> {
        Ok(UnixSendTo {
            io_data: socket.0.as_io_data(),
            buf,
            socket: socket.0.inner(),
            path,
            timeout: socket.write_timeout().unwrap(),
        })
    }

    pub fn done(&mut self) -> io::Result<usize> {
        loop {
            co_io_result()?;

            if !self.io_data.is_write_wait() || self.io_data.is_write_ready() {
                self.io_data.reset_write();
                self.io_data.clear_write_wait();

                match self.socket.send_to(self.buf, self.path) {
                    Ok(n) => return Ok(n),
                    #[cold]
                    Err(e) => {
                        // raw_os_error is faster than kind
                        let raw_err = e.raw_os_error();
                        if raw_err == Some(libc::EAGAIN) || raw_err == Some(libc::EWOULDBLOCK) {
                            self.io_data.set_write_wait();
                        } else {
                            return Err(e);
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

impl<'a> EventSource for UnixSendTo<'a> {
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
