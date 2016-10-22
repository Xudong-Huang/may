use std::{self, io};
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use io::AsEventData;
use yield_now::yield_with;
use net::{TcpStream, TcpListener};
use coroutine::{CoroutineImpl, EventSource};
use super::super::{EventData, co_io_result};

pub struct TcpListenerAccept<'a> {
    io_data: &'a mut EventData,
    socket: &'a std::net::TcpListener,
}

impl<'a> TcpListenerAccept<'a> {
    pub fn new(socket: &'a TcpListener) -> io::Result<Self> {
        let io_data = socket.as_event_data();
        // clear the io_flag
        // io_data.io_flag.store(0, Ordering::Relaxed);
        Ok(TcpListenerAccept {
            io_data: io_data,
            socket: socket.inner(),
        })
    }

    #[inline]
    pub fn done(self) -> io::Result<(TcpStream, SocketAddr)> {
        loop {
            try!(co_io_result());

            // clear the io_flag
            self.io_data.io_flag.store(false, Ordering::Relaxed);

            match self.socket.accept() {
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
                ret @ _ => return ret.and_then(|(s, a)| TcpStream::new(s).map(|s| (s, a))),
            }

            // clear the events
            if self.io_data.io_flag.swap(false, Ordering::Relaxed) {
                continue;
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

        // there is no event
        if !self.io_data.io_flag.load(Ordering::Relaxed) {
            return;
        }

        // since we got data here, need to remove the timer handle and schedule
        self.io_data.schedule();
    }
}
