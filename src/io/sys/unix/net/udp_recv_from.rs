use std::{self, io};
use std::time::Duration;
use std::net::SocketAddr;
use std::os::unix::io::AsRawFd;
use net::UdpSocket;
use yield_now::yield_with;
use scheduler::get_scheduler;
use coroutine::{CoroutineImpl, EventSource};
use super::super::{EventData, FLAG_READ, co_io_result};

pub struct UdpRecvFrom<'a> {
    io_data: EventData,
    buf: &'a mut [u8],
    socket: &'a std::net::UdpSocket,
    timeout: Option<Duration>,
}

impl<'a> UdpRecvFrom<'a> {
    pub fn new(socket: &'a UdpSocket, buf: &'a mut [u8]) -> Self {
        UdpRecvFrom {
            io_data: EventData::new(socket.as_raw_fd(), FLAG_READ),
            buf: buf,
            socket: socket.inner(),
            timeout: socket.read_timeout().unwrap(),
        }
    }

    #[inline]
    pub fn done(self) -> io::Result<(usize, SocketAddr)> {
        let s = get_scheduler().get_selector();
        loop {
            s.del_fd(self.io_data.fd);
            try!(co_io_result());

            match self.socket.recv_from(self.buf) {
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
                ret @ _ => return ret,
            }

            // the result is still WouldBlock, need to try again
            yield_with(&self);
        }
    }
}

impl<'a> EventSource for UdpRecvFrom<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        let selector = s.get_selector();
        selector.add_io_timer(&mut self.io_data, self.timeout);
        self.io_data.co = Some(co);

        // register the io operaton
        co_try!(s,
                self.io_data.co.take().expect("can't get co"),
                selector.add_io(&self.io_data));
    }
}
