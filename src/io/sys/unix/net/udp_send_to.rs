use std::{self, io};
use std::time::Duration;
use std::net::ToSocketAddrs;
use std::os::unix::io::AsRawFd;
use super::co_io_result;
use super::super::{EventData, FLAG_WRITE};
use net::UdpSocket;
use yield_now::yield_with;
use scheduler::get_scheduler;
use coroutine::{CoroutineImpl, EventSource};

pub struct UdpSendTo<'a, A: ToSocketAddrs> {
    io_data: EventData,
    buf: &'a [u8],
    socket: &'a std::net::UdpSocket,
    addr: A,
    timeout: Option<Duration>,
}

impl<'a, A: ToSocketAddrs> UdpSendTo<'a, A> {
    pub fn new(socket: &'a UdpSocket, buf: &'a [u8], addr: A) -> io::Result<Self> {
        Ok(UdpSendTo {
            io_data: EventData::new(socket.as_raw_fd(), FLAG_WRITE),
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
            match self.socket.send_to(self.buf, &self.addr) {
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
                ret @ _ => return ret,
            }

            // the result is still WouldBlock, need to try again
            yield_with(&self);
        }
    }
}

impl<'a, A: ToSocketAddrs> EventSource for UdpSendTo<'a, A> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        s.add_io_timer(&mut self.io_data, self.timeout);
        self.io_data.co = Some(co);
    }
}
