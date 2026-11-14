use std::io;
use std::os::unix::net::SocketAddr;
use std::sync::atomic::Ordering;
#[cfg(feature = "io_timeout")]
use std::time::Duration;

use super::super::{co_io_result, IoData};
#[cfg(feature = "io_cancel")]
use crate::coroutine_impl::co_cancel_data;
use crate::coroutine_impl::{is_coroutine, CoroutineImpl, EventSource};
use crate::io::AsIoData;
use crate::os::unix::net::UnixDatagram;
use crate::yield_now::yield_with_io;

pub struct UnixRecvFrom<'a> {
    io_data: &'a IoData,
    buf: &'a mut [u8],
    socket: &'a std::os::unix::net::UnixDatagram,
    #[cfg(feature = "io_timeout")]
    timeout: Option<Duration>,
    pub(crate) is_coroutine: bool,
}

impl<'a> UnixRecvFrom<'a> {
    pub fn new(socket: &'a UnixDatagram, buf: &'a mut [u8]) -> Self {
        UnixRecvFrom {
            io_data: socket.0.as_io_data(),
            buf,
            socket: socket.0.inner(),
            #[cfg(feature = "io_timeout")]
            timeout: socket.0.read_timeout().unwrap(),
            is_coroutine: is_coroutine(),
        }
    }

    pub fn done(&mut self) -> io::Result<(usize, SocketAddr)> {
        loop {
            co_io_result(self.is_coroutine)?;

            // clear the io_flag
            self.io_data.io_flag.store(0, Ordering::Relaxed);

            match self.socket.recv_from(self.buf) {
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

            if self.io_data.io_flag.load(Ordering::Relaxed) != 0 {
                continue;
            }

            // the result is still WouldBlock, need to try again
            yield_with_io(self, self.is_coroutine);
        }
    }
}

impl EventSource for UnixRecvFrom<'_> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        #[cfg(feature = "io_cancel")]
        let cancel = co_cancel_data(&co);
        let io_data = self.io_data;

        // the timer holds its own clone of the wait slot, so an immediate
        // expiry still reaches the coroutine; after publishing, only use the
        // shared event data, the resumed coroutine may drop the io data
        let (event_data, wait) = io_data.publish_co(
            co,
            #[cfg(feature = "io_timeout")]
            self.timeout,
        );

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
