use std::{self, io};
use std::net::SocketAddr;
use std::os::unix::io::AsRawFd;
use super::co_io_result;
use super::super::{EventData, FLAG_READ};
use yield_now::yield_with;
// use scheduler::get_scheduler;
use net::{TcpStream, TcpListener};
use coroutine::{CoroutineImpl, EventSource};


pub struct TcpListenerAccept<'a> {
    io_data: EventData,
    socket: &'a std::net::TcpListener,
}

impl<'a> TcpListenerAccept<'a> {
    pub fn new(socket: &'a TcpListener) -> io::Result<Self> {
        Ok(TcpListenerAccept {
            io_data: EventData::new(socket.as_raw_fd(), FLAG_READ),
            socket: socket.inner(),
        })
    }

    #[inline]
    pub fn done(self) -> io::Result<(TcpStream, SocketAddr)> {
        loop {
            try!(co_io_result());
            match self.socket.accept() {
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e),
                Ok((s, a)) => return TcpStream::new(s).map(|s| (s, a)),
            }

            // the result is still WouldBlock, need to try again
            yield_with(&self);
        }
    }
}

impl<'a> EventSource for TcpListenerAccept<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        // if there is no timer we don't need to call add_io_timer
        // let s = get_scheduler();
        // s.add_io_timer(&mut self.io_data, None);
        self.io_data.co = Some(co);
    }
}
