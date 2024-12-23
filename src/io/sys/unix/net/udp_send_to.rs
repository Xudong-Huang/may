use std::io;
use std::net::ToSocketAddrs;
use std::sync::atomic::Ordering;
#[cfg(feature = "io_timeout")]
use std::time::Duration;

use super::super::{co_io_result, IoData};
use crate::coroutine_impl::{is_coroutine, CoroutineImpl, EventSource};
use crate::io::AsIoData;
use crate::net::UdpSocket;
use crate::yield_now::yield_with_io;

pub struct UdpSendTo<'a, A: ToSocketAddrs> {
    io_data: &'a IoData,
    buf: &'a [u8],
    socket: &'a std::net::UdpSocket,
    addr: A,
    #[cfg(feature = "io_timeout")]
    timeout: Option<Duration>,
    pub(crate) is_coroutine: bool,
}

impl<'a, A: ToSocketAddrs> UdpSendTo<'a, A> {
    pub fn new(socket: &'a UdpSocket, buf: &'a [u8], addr: A) -> io::Result<Self> {
        Ok(UdpSendTo {
            io_data: socket.as_io_data(),
            buf,
            socket: socket.inner(),
            addr,
            #[cfg(feature = "io_timeout")]
            timeout: socket.write_timeout().unwrap(),
            is_coroutine: is_coroutine(),
        })
    }

    pub fn done(&mut self) -> io::Result<usize> {
        loop {
            co_io_result(self.is_coroutine)?;

            // clear the io_flag
            self.io_data.io_flag.store(0, Ordering::Relaxed);

            match self.socket.send_to(self.buf, &self.addr) {
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

impl<A: ToSocketAddrs> EventSource for UdpSendTo<'_, A> {
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
