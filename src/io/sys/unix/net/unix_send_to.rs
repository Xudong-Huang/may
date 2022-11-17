use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::{self, io};

use super::super::{co_io_result, IoData};
use crate::coroutine_impl::{is_coroutine, CoroutineImpl, EventSource};
use crate::io::AsIoData;
use crate::os::unix::net::UnixDatagram;
use crate::scheduler::get_scheduler;
use crate::yield_now::yield_with_io;

pub struct UnixSendTo<'a> {
    io_data: &'a IoData,
    buf: &'a [u8],
    socket: &'a std::os::unix::net::UnixDatagram,
    path: &'a Path,
    timeout: Option<Duration>,
    pub(crate) is_coroutine: bool,
}

impl<'a> UnixSendTo<'a> {
    pub fn new(socket: &'a UnixDatagram, buf: &'a [u8], path: &'a Path) -> io::Result<Self> {
        Ok(UnixSendTo {
            io_data: socket.0.as_io_data(),
            buf,
            socket: socket.0.inner(),
            path,
            timeout: socket.write_timeout().unwrap(),
            is_coroutine: is_coroutine(),
        })
    }

    pub fn done(&mut self) -> io::Result<usize> {
        loop {
            co_io_result(self.is_coroutine)?;

            // clear the io_flag
            self.io_data.io_flag.store(false, Ordering::Relaxed);

            match self.socket.send_to(self.buf, self.path) {
                Ok(n) => return Ok(n),
                Err(e) => {
                    // raw_os_error is faster than kind
                    let raw_err = e.raw_os_error();
                    if raw_err == Some(libc::EAGAIN) || raw_err == Some(libc::EWOULDBLOCK) {
                        // do nothing here
                    } else {
                        return Err(e);
                    }
                }
            }

            if self.io_data.io_flag.swap(false, Ordering::Relaxed) {
                continue;
            }

            // the result is still WouldBlock, need to try again
            yield_with_io(self, self.is_coroutine);
        }
    }
}

impl<'a> EventSource for UnixSendTo<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let io_data = (*self.io_data).clone();

        if let Some(dur) = self.timeout {
            get_scheduler()
                .get_selector()
                .add_io_timer(self.io_data, dur);
        }
        self.io_data.co.swap(co, Ordering::Release);

        // there is event, re-run the coroutine
        if io_data.io_flag.load(Ordering::Acquire) {
            io_data.schedule();
        }
    }
}
