use std::io::{self, IoSlice};
use std::sync::atomic::Ordering;
#[cfg(feature = "io_timeout")]
use std::time::Duration;

use super::super::{co_io_result, IoData};
use crate::coroutine_impl::{is_coroutine, CoroutineImpl, EventSource};
use crate::io::AsIoData;
use crate::yield_now::yield_with_io;

pub struct SocketWriteVectored<'a> {
    io_data: &'a IoData,
    bufs: &'a [IoSlice<'a>],
    socket: &'a std::net::TcpStream,
    #[cfg(feature = "io_timeout")]
    timeout: Option<Duration>,
    pub(crate) is_coroutine: bool,
}

impl<'a> SocketWriteVectored<'a> {
    pub fn new<T: AsIoData>(
        s: &'a T,
        socket: &'a std::net::TcpStream,
        bufs: &'a [IoSlice<'a>],
        #[cfg(feature = "io_timeout")] timeout: Option<Duration>,
    ) -> Self {
        SocketWriteVectored {
            io_data: s.as_io_data(),
            bufs,
            socket,
            #[cfg(feature = "io_timeout")]
            timeout,
            is_coroutine: is_coroutine(),
        }
    }

    pub fn done(&mut self) -> io::Result<usize> {
        use std::io::Write;

        loop {
            co_io_result(self.is_coroutine)?;

            // clear the io_flag
            self.io_data.io_flag.store(0, Ordering::Relaxed);

            match self.socket.write_vectored(self.bufs) {
                Ok(n) => return Ok(n),
                Err(e) => {
                    let raw_err = e.raw_os_error();
                    if raw_err == Some(libc::EAGAIN) || raw_err == Some(libc::EWOULDBLOCK) {
                        // do nothing here
                    } else {
                        return Err(e);
                    }
                }
            }

            if self.io_data.io_flag.load(Ordering::Relaxed) != 0 {
                continue;
            }

            // the result is still WouldBlock, need to try again
            yield_with_io(self, self.is_coroutine);
        }
    }
}

impl EventSource for SocketWriteVectored<'_> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let io_data = self.io_data;

        #[cfg(feature = "io_timeout")]
        if let Some(dur) = self.timeout {
            crate::scheduler::get_scheduler()
                .get_selector()
                .add_io_timer(self.io_data, dur);
        }
        io_data.co.store(co);

        // there is event, re-run the coroutine
        if io_data.io_flag.load(Ordering::Acquire) != 0 {
            io_data.fast_schedule();
        }
    }
}
