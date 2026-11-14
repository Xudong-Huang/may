use std::io;
use std::os::fd::BorrowedFd;
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
        let fd = unsafe { BorrowedFd::borrow_raw(self.io_data.fd) };
        loop {
            co_io_result(self.is_coroutine)?;

            // clear the io_flag
            self.io_data.io_flag.store(0, Ordering::Relaxed);

            // finish the read operation
            match read(fd, self.buf) {
                Ok(n) => return Ok(n),
                Err(e) => {
                    if e == nix::errno::Errno::EAGAIN {
                        // do nothing
                    } else {
                        return Err(from_nix_error(e));
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

impl EventSource for SocketRead<'_> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        #[cfg(feature = "io_cancel")]
        let cancel = co_cancel_data(&co);
        let io_data = self.io_data;

        // the wait owns its own shared slot, so a timer that fires immediately
        // still finds the coroutine; after publishing, only use the shared
        // event data, the resumed coroutine may drop the io data
        let (event_data, wait) = io_data.publish_co(
            co,
            #[cfg(feature = "io_timeout")]
            self.timeout,
        );
        // till here the io may be done in other thread

        // there is event, re-run the coroutine
        if event_data.io_flag.load(Ordering::Acquire) != 0 {
            #[allow(clippy::needless_return)]
            return event_data.fast_schedule(&wait);
        }

        #[cfg(feature = "io_cancel")]
        {
            // register the cancel io data
            cancel.set_io(event_data);
            // re-check the cancel status
            if cancel.is_canceled() {
                cancel.cancel();
            }
        }
    }
}
