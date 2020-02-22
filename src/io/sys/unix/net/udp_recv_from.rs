use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::{self, io};

use super::super::{co_io_result, IoData};
use crate::coroutine_impl::{co_cancel_data, CoroutineImpl, EventSource};
use crate::io::AsIoData;
use crate::net::UdpSocket;
use crate::scheduler::get_scheduler;
use crate::yield_now::yield_with;

pub struct UdpRecvFrom<'a> {
    io_data: &'a IoData,
    buf: &'a mut [u8],
    socket: &'a std::net::UdpSocket,
    timeout: Option<Duration>,
}

impl<'a> UdpRecvFrom<'a> {
    pub fn new(socket: &'a UdpSocket, buf: &'a mut [u8]) -> Self {
        UdpRecvFrom {
            io_data: socket.as_io_data(),
            buf,
            socket: socket.inner(),
            timeout: socket.read_timeout().unwrap(),
        }
    }

    pub fn done(&mut self) -> io::Result<(usize, SocketAddr)> {
        loop {
            co_io_result()?;

            if !self.io_data.is_read_wait() || self.io_data.is_read_ready() {
                self.io_data.reset_read();

                match self.socket.recv_from(self.buf) {
                    Ok(n) => return Ok(n),
                    #[cold]
                    Err(e) => {
                        // raw_os_error is faster than kind
                        let raw_err = e.raw_os_error();
                        if raw_err == Some(libc::EAGAIN) || raw_err == Some(libc::EWOULDBLOCK) {
                            self.io_data.set_read_wait()
                        } else {
                            return Err(e);
                        }
                    }
                }
            }

            if (self.io_data.io_flag.fetch_and(!1, Ordering::Relaxed) & 1) != 0 {
                continue;
            }

            // the result is still WouldBlock, need to try again
            yield_with(self);
        }
    }
}

impl<'a> EventSource for UdpRecvFrom<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let cancel = co_cancel_data(&co);

        if let Some(dur) = self.timeout {
            get_scheduler()
                .get_selector()
                .add_io_timer(self.io_data, dur);
        }
        self.io_data.co.swap(co, Ordering::Release);

        // there is event, re-run the coroutine
        if self.io_data.is_read_ready() {
            return self.io_data.schedule();
        }

        // register the cancel io data
        cancel.set_io((*self.io_data).clone());
        // re-check the cancel status
        if cancel.is_canceled() {
            unsafe { cancel.cancel() };
        }
    }
}
