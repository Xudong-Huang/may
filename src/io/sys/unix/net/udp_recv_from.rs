use std::{self, io};
use std::time::Duration;
use std::net::SocketAddr;
use std::os::unix::io::{AsRawFd, RawFd};
use super::co_io_result;
use super::super::from_nix_error;
use super::super::{EventData, FLAG_READ};
use net::UdpSocket;
use yield_now::yield_with;
use scheduler::get_scheduler;
use coroutine::{CoroutineImpl, EventSource};

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
        loop {
            try!(co_io_result());
            match self.socket.recv_from(self.buf) {
                Err(err) => {
                    if err.kind() != io::ErrorKind::WouldBlock {
                        return Err(err);
                    }
                }
                ret @ Ok(..) => {
                    return ret;
                }
            }

            // the result is still WouldBlock, need to try again
            yield_with(&self);
        }
    }
}

impl<'a> EventSource for UdpRecvFrom<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        // prepare the co first
        self.io_data.co = Some(co);

        // register the io operaton
        co_try!(s,
                self.io_data.co.take().unwrap(),
                s.add_io(&mut self.io_data, self.timeout));
    }
}
