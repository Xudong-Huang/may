use std::io;
use std::sync::atomic::Ordering;
#[cfg(feature = "io_timeout")]
use std::time::Duration;

use super::super::{co_io_result, from_nix_error, IoData};
#[cfg(feature = "io_cancel")]
use crate::coroutine_impl::co_cancel_data;
use crate::coroutine_impl::{is_coroutine, CoroutineImpl, EventSource};
use crate::io::AsIoData;
use crate::yield_now::yield_with_io;
use nix::unistd::read;

pub struct SocketRead<'a> {
    io_data: &'a IoData,
    buf: &'a mut [u8],
    #[cfg(feature = "io_timeout")]
    timeout: Option<Duration>,
    pub(crate) is_coroutine: bool,
}

impl<'a> SocketRead<'a> {
    pub fn new<T: AsIoData>(
        s: &'a T,
        buf: &'a mut [u8],
        #[cfg(feature = "io_timeout")] timeout: Option<Duration>,
    ) -> Self {
        SocketRead {
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

            // finish the read operation
            match read(self.io_data.fd, self.buf) {
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

impl<'a> EventSource for SocketRead<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        #[cfg(feature = "io_cancel")]
        let cancel = co_cancel_data(&co);
        let io_data = self.io_data;

        #[cfg(feature = "io_timeout")]
        if let Some(dur) = self.timeout {
            crate::scheduler::get_scheduler()
                .get_selector()
                .add_io_timer(self.io_data, dur);
        }

        // after register the coroutine, it's possible that other thread run it immediately
        // and cause the process after it invalid, this is kind of user and kernel competition
        // so we need to delay the drop of the EventSource, that's why _g is here
        unsafe { io_data.co.unsync_store(co) };
        // till here the io may be done in other thread

        // there is event, re-run the coroutine
        if io_data.io_flag.load(Ordering::Acquire) {
            #[allow(clippy::needless_return)]
            return io_data.fast_schedule();
        }

        #[cfg(feature = "io_cancel")]
        {
            // register the cancel io data
            cancel.set_io((*io_data).clone());
            // re-check the cancel status
            if cancel.is_canceled() {
                unsafe { cancel.cancel() };
            }
        }
    }
}
