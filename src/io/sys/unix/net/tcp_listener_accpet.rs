use std::{self, io};
use std::ops::Deref;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use super::super::{IoData, co_io_result};
use io::AsIoData;
use cancel::Cancel;
use yield_now::yield_with;
use io::cancel::CancelIoImpl;
use net::{TcpStream, TcpListener};
use coroutine::{CoroutineImpl, EventSource, get_cancel_data};

pub struct TcpListenerAccept<'a> {
    io_data: &'a IoData,
    socket: &'a std::net::TcpListener,
    io_cancel: &'static Cancel<CancelIoImpl>,
}

impl<'a> TcpListenerAccept<'a> {
    pub fn new(socket: &'a TcpListener) -> io::Result<Self> {
        Ok(TcpListenerAccept {
            io_data: socket.as_io_data(),
            socket: socket.inner(),
            io_cancel: get_cancel_data(),
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
                ret => return ret.and_then(|(s, a)| TcpStream::new(s).map(|s| (s, a))),
            }

            if self.io_data.io_flag.swap(false, Ordering::Relaxed) {
                continue;
            }

            // the result is still WouldBlock, need to try again
            yield_with(&self);
        }
    }
}

impl<'a> EventSource for TcpListenerAccept<'a> {
    fn get_cancel_data(&self) -> Option<&Cancel<CancelIoImpl>> {
        Some(self.io_cancel)
    }

    fn subscribe(&mut self, co: CoroutineImpl) {
        // if there is no timer we don't need to call add_io_timer
        self.io_data.co.swap(co, Ordering::Release);

        // there is event happened
        if self.io_data.io_flag.load(Ordering::Relaxed) {
            return self.io_data.schedule();
        }

        // deal with the cancel
        self.get_cancel_data().map(|cancel| {
            // register the cancel io data
            cancel.set_io(self.io_data.deref().clone());
            // re-check the cancel status
            if cancel.is_canceled() {
                unsafe { cancel.cancel() };
            }
        });
    }
}
