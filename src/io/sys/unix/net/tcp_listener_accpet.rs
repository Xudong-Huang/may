use std::{self, io};
use std::net::SocketAddr;
use std::os::unix::io::AsRawFd;
use super::super::{EventData, FLAG_READ, co_io_result};
use yield_now::yield_with;
use scheduler::get_scheduler;
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
        let s = get_scheduler().get_selector();
        loop {
            s.del_fd(self.io_data.fd);
            try!(co_io_result());

            match self.socket.accept() {
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
                ret @ _ => return ret.and_then(|(s, a)| TcpStream::new(s).map(|s| (s, a))),
            }

            // the result is still WouldBlock, need to try again
            yield_with(&self);
        }
    }
}

impl<'a> EventSource for TcpListenerAccept<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        // if there is no timer we don't need to call add_io_timer
        self.io_data.co = Some(co);

        // register the io operaton
        co_try!(s,
                self.io_data.co.take().expect("can't get co"),
                s.get_selector().add_io(&self.io_data));
    }
}
