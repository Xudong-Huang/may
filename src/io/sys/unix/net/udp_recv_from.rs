use std::net::SocketAddr;
use std::sync::atomic::Ordering;
#[cfg(feature = "io_timeout")]
use std::time::Duration;
use std::{self, io};

use super::super::{co_io_result, IoData};
#[cfg(feature = "io_cancel")]
use crate::coroutine_impl::co_cancel_data;
use crate::coroutine_impl::{is_coroutine, CoroutineImpl, EventSource};
use crate::io::AsIoData;
use crate::net::UdpSocket;
use crate::yield_now::yield_with_io;

pub struct UdpRecvFrom<'a> {
    io_data: &'a IoData,
    buf: &'a mut [u8],
    socket: &'a std::net::UdpSocket,
    #[cfg(feature = "io_timeout")]
    timeout: Option<Duration>,
    pub(crate) is_coroutine: bool,
}

impl<'a> UdpRecvFrom<'a> {
    pub fn new(socket: &'a UdpSocket, buf: &'a mut [u8]) -> Self {
        UdpRecvFrom {
            io_data: socket.as_io_data(),
            buf,
            socket: socket.inner(),
            #[cfg(feature = "io_timeout")]
            timeout: socket.read_timeout().unwrap(),
            is_coroutine: is_coroutine(),
        }
    }

    pub fn done(&mut self) -> io::Result<(usize, SocketAddr)> {
        loop {
            co_io_result(self.is_coroutine)?;

            // clear the io_flag
            self.io_data.io_flag.store(false, Ordering::Relaxed);

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

            if self.io_data.io_flag.swap(false, Ordering::Relaxed) {
                continue;
            }

            // the result is still WouldBlock, need to try again
            yield_with_io(self, self.is_coroutine);
        }
    }
}

impl<'a> EventSource for UdpRecvFrom<'a> {
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
        io_data.co.store(co);

        // there is event, re-run the coroutine
        if io_data.io_flag.load(Ordering::Acquire) {
            #[allow(clippy::needless_return)]
            return io_data.schedule();
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
