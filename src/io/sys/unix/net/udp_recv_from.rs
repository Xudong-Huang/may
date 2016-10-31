use std::{self, io};
use std::time::Duration;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use net::UdpSocket;
use io::AsEventData;
use yield_now::yield_with;
use scheduler::get_scheduler;
use coroutine::{CoroutineImpl, EventSource};
use super::super::{EventData, co_io_result};

pub struct UdpRecvFrom<'a> {
    io_data: &'a mut EventData,
    buf: &'a mut [u8],
    socket: &'a std::net::UdpSocket,
    timeout: Option<Duration>,
}

impl<'a> UdpRecvFrom<'a> {
    pub fn new(socket: &'a UdpSocket, buf: &'a mut [u8]) -> Self {
        UdpRecvFrom {
            io_data: socket.as_event_data(),
            buf: buf,
            socket: socket.inner(),
            timeout: socket.read_timeout().unwrap(),
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
    fn subscribe(&mut self, co: CoroutineImpl) {
        get_scheduler().get_selector().add_io_timer(&mut self.io_data, self.timeout);
        self.io_data.co.swap(co, Ordering::Release);

        // there is no event, let the selector invoke it
        if !self.io_data.io_flag.load(Ordering::Relaxed) {
            return;
        }

        // since we got data here, need to remove the timer handle and schedule
        self.io_data.schedule();
    }
}
