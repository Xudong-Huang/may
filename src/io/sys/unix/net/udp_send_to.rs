use std::net::ToSocketAddrs;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::{self, io};

use super::super::{co_io_result, IoData};
use crate::coroutine_impl::{CoroutineImpl, EventSource};
use crate::io::AsIoData;
use crate::net::UdpSocket;
use crate::scheduler::get_scheduler;
use crate::sync::delay_drop::DelayDrop;
use crate::yield_now::yield_with;

pub struct UdpSendTo<'a, A: ToSocketAddrs> {
    io_data: &'a IoData,
    buf: &'a [u8],
    socket: &'a std::net::UdpSocket,
    addr: A,
    timeout: Option<Duration>,
    can_drop: DelayDrop,
}

impl<'a, A: ToSocketAddrs> UdpSendTo<'a, A> {
    pub fn new(socket: &'a UdpSocket, buf: &'a [u8], addr: A) -> io::Result<Self> {
        Ok(UdpSendTo {
            io_data: socket.as_io_data(),
            buf,
            socket: socket.inner(),
            addr,
            timeout: socket.write_timeout().unwrap(),
            can_drop: DelayDrop::new(),
        })
    }

    pub fn done(&mut self) -> io::Result<usize> {
        loop {
            co_io_result()?;

            // clear the io_flag
            self.io_data.io_flag.store(false, Ordering::Relaxed);

            match self.socket.send_to(self.buf, &self.addr) {
                Ok(n) => return Ok(n),
                #[cold]
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
            self.can_drop.reset();
            yield_with(self);
        }
    }
}

impl<'a, A: ToSocketAddrs> EventSource for UdpSendTo<'a, A> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let _g = self.can_drop.delay_drop();
        if let Some(dur) = self.timeout {
            get_scheduler()
                .get_selector()
                .add_io_timer(self.io_data, dur);
        }
        self.io_data.co.swap(co, Ordering::Release);

        // there is event, re-run the coroutine
        if self.io_data.io_flag.load(Ordering::Acquire) {
            self.io_data.schedule();
        }
    }
}
