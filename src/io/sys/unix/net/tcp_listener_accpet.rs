use std::{self, io};
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use super::super::{EventData, FLAG_READ, co_io_result};
use io::AsEventData;
use yield_now::yield_with;
use net::{TcpStream, TcpListener};
use coroutine::{CoroutineImpl, EventSource};


pub struct TcpListenerAccept<'a> {
    io_data: &'a mut EventData,
    socket: &'a std::net::TcpListener,
    ret: Option<io::Result<(::std::net::TcpStream, SocketAddr)>>,
}

impl<'a> TcpListenerAccept<'a> {
    pub fn new(socket: &'a TcpListener) -> io::Result<Self> {
        let io_data = socket.as_event_data();
        io_data.interest = FLAG_READ;
        Ok(TcpListenerAccept {
            io_data: io_data,
            socket: socket.inner(),
            ret: None,
        })
    }

    #[inline]
    pub fn done(self) -> io::Result<(TcpStream, SocketAddr)> {
        loop {
            try!(co_io_result());
            match self.ret {
                // we already got the ret in subscribe
                Some(ret) => return ret.and_then(|(s, a)| TcpStream::new(s).map(|s| (s, a))),
                None => {}
            }

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
        // if there is no timer we don't need to call add_io_timer
        self.io_data.co.swap(co, Ordering::Release);

        match self.socket.accept() {
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => return,
            ret @ _ => self.ret = Some(ret),
        }

        // since we got data here, need to remove the timer handle and schedule
        self.io_data.schedule();
    }
}
