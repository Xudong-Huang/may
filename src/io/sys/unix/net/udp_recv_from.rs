use std::{self, io};
use std::ops::Deref;
use std::time::Duration;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use super::super::{IoData, co_io_result};
use io::AsIoData;
use cancel::Cancel;
use net::UdpSocket;
use yield_now::yield_with;
use io::cancel::CancelIoImpl;
use scheduler::get_scheduler;
use coroutine::{CoroutineImpl, EventSource, get_cancel_data};

pub struct UdpRecvFrom<'a> {
    io_data: &'a IoData,
    buf: &'a mut [u8],
    socket: &'a std::net::UdpSocket,
    timeout: Option<Duration>,
    io_cancel: &'static Cancel<CancelIoImpl>,
}

impl<'a> UdpRecvFrom<'a> {
    pub fn new(socket: &'a UdpSocket, buf: &'a mut [u8]) -> Self {
        UdpRecvFrom {
            io_data: socket.as_io_data(),
            buf: buf,
            socket: socket.inner(),
            timeout: socket.read_timeout().unwrap(),
            io_cancel: get_cancel_data(),
        }
    }

    #[inline]
    pub fn done(self) -> io::Result<(usize, SocketAddr)> {
        loop {
            try!(co_io_result());

            // clear the io_flag
            self.io_data.io_flag.store(false, Ordering::Relaxed);

            match self.socket.recv_from(self.buf) {
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

impl<'a> EventSource for UdpRecvFrom<'a> {
    fn get_cancel_data(&self) -> Option<&Cancel<CancelIoImpl>> {
        Some(self.io_cancel)
    }

    fn subscribe(&mut self, co: CoroutineImpl) {
        get_scheduler().get_selector().add_io_timer(self.io_data, self.timeout);
        self.io_data.co.swap(co, Ordering::Release);

        // there is event, re-run the coroutine
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
