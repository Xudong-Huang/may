use std::io;
use std::sync::atomic::Ordering;
#[cfg(feature = "io_timeout")]
use std::time::Duration;

use super::super::{co_io_result, from_nix_error, IoData};
use crate::coroutine_impl::{is_coroutine, CoroutineImpl, EventSource};
use crate::io::AsIoData;
use crate::yield_now::yield_with_io;

use nix::unistd::write;

pub struct SocketWrite<'a> {
    io_data: &'a IoData,
    buf: &'a [u8],
    #[cfg(feature = "io_timeout")]
    timeout: Option<Duration>,
    pub(crate) is_coroutine: bool,
}

impl<'a> SocketWrite<'a> {
    pub fn new<T: AsIoData>(
        s: &'a T,
        buf: &'a [u8],
        #[cfg(feature = "io_timeout")] timeout: Option<Duration>,
    ) -> Self {
        SocketWrite {
            io_data: s.as_io_data(),
            buf,
            #[cfg(feature = "io_timeout")]
            timeout,
            is_coroutine: is_coroutine(),
        }
    }

    pub fn done(&mut self) -> io::Result<usize> {
        loop {
            co_io_result(self.is_coroutine)?;

            // clear the io_flag
            self.io_data.io_flag.store(false, Ordering::Relaxed);

            match write(self.io_data, self.buf) {
                Ok(n) => return Ok(n),
                Err(e) => {
                    if e == nix::errno::Errno::EAGAIN {
                        // do nothing
                    } else {
                        return Err(from_nix_error(e));
                    }
                }
            }

            if self.io_data.io_flag.load(Ordering::Relaxed) {
                continue;
            }

            // the result is still WouldBlock, need to try again
            yield_with_io(self, self.is_coroutine);
        }
    }
}

impl<'a> EventSource for SocketWrite<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let io_data = self.io_data;

        #[cfg(feature = "io_timeout")]
        if let Some(dur) = self.timeout {
            crate::scheduler::get_scheduler()
                .get_selector()
                .add_io_timer(self.io_data, dur);
        }
        unsafe { io_data.co.unsync_store(co) };

        // there is event, re-run the coroutine
        if io_data.io_flag.load(Ordering::Acquire) {
            io_data.fast_schedule();
        }
    }
}
