use std::{self, io};
use std::time::Duration;
use std::net::{ToSocketAddrs, SocketAddr};
use std::os::unix::io::{AsRawFd, RawFd};
use super::co_io_result;
use super::super::from_nix_error;
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
            match self.socket.send_to(self.buf, self.addr) {
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

impl<'a, A: ToSocketAddrs> EventSource for UdpSendTo<'a, A> {
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
