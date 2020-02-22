use std::io::{self, IoSlice};
use std::sync::atomic::Ordering;
use std::time::Duration;

use super::super::{co_io_result, IoData};
use crate::coroutine_impl::{CoroutineImpl, EventSource};
use crate::io::AsIoData;
use crate::scheduler::get_scheduler;
use crate::yield_now::yield_with;

pub struct SocketWriteVectored<'a> {
    io_data: &'a IoData,
    bufs: &'a [IoSlice<'a>],
    socket: &'a std::net::TcpStream,
    timeout: Option<Duration>,
}

impl<'a> SocketWriteVectored<'a> {
    pub fn new<T: AsIoData>(
        s: &'a T,
        socket: &'a std::net::TcpStream,
        bufs: &'a [IoSlice<'a>],
        timeout: Option<Duration>,
    ) -> Self {
        SocketWriteVectored {
            io_data: s.as_io_data(),
            bufs,
            socket,
            timeout,
        }
    }

    pub fn done(&mut self) -> io::Result<usize> {
        use std::io::Write;

        loop {
            co_io_result()?;

            if !self.io_data.is_write_wait() || self.io_data.is_write_ready() {
                self.io_data.reset_write();

                match self.socket.write_vectored(self.bufs) {
                    Ok(n) => return Ok(n),
                    #[cold]
                    Err(e) => {
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

impl<'a> EventSource for SocketWriteVectored<'a> {
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
