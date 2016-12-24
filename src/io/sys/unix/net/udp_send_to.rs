use std::{self, io};
use std::time::Duration;
use std::net::ToSocketAddrs;
use std::sync::atomic::Ordering;
use super::super::{IoData, co_io_result};
use io::AsIoData;
use net::UdpSocket;
use yield_now::yield_with;
use scheduler::get_scheduler;
use coroutine::{CoroutineImpl, EventSource};

pub struct UdpSendTo<'a, A: ToSocketAddrs> {
    io_data: &'a IoData,
    buf: &'a [u8],
    socket: &'a std::net::UdpSocket,
    addr: A,
    timeout: Option<Duration>,
}

impl<'a, A: ToSocketAddrs> UdpSendTo<'a, A> {
    pub fn new(socket: &'a UdpSocket, buf: &'a [u8], addr: A) -> io::Result<Self> {
        Ok(UdpSendTo {
            io_data: socket.as_io_data(),
            buf: buf,
            socket: socket.inner(),
            addr: addr,
            timeout: socket.write_timeout().unwrap(),
        })
    }

    #[inline]
    pub fn done(self) -> io::Result<usize> {
        loop {
            try!(co_io_result());

            // clear the io_flag
            self.io_data.io_flag.store(false, Ordering::Relaxed);

            match self.socket.send_to(self.buf, &self.addr) {
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
                ret => return ret,
            }

            if self.io_data.io_flag.swap(false, Ordering::Relaxed) {
                continue;
            }

            // the result is still WouldBlock, need to try again
            yield_with(&self);
        }
    }
}

impl<'a, A: ToSocketAddrs> EventSource for UdpSendTo<'a, A> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        get_scheduler().get_selector().add_io_timer(self.io_data, self.timeout);
        self.io_data.co.swap(co, Ordering::Release);

        // there is event, re-run the coroutine
        if self.io_data.io_flag.load(Ordering::Relaxed) {
            self.io_data.schedule();
        }
    }
}
