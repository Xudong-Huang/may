use std::io;
use std::time::Duration;
use std::sync::atomic::Ordering;
use super::super::from_nix_error;
use super::super::nix::unistd::read;
use super::super::{EventData, FLAG_READ, co_io_result};
use io::AsEventData;
use yield_now::yield_with;
use scheduler::get_scheduler;
use coroutine::{CoroutineImpl, EventSource};

pub struct SocketRead<'a> {
    io_data: &'a mut EventData,
    buf: &'a mut [u8],
    ret: Option<io::Result<usize>>,
    timeout: Option<Duration>,
}

impl<'a> SocketRead<'a> {
    pub fn new<T: AsEventData>(s: &'a T, buf: &'a mut [u8], timeout: Option<Duration>) -> Self {
        let io_data = s.as_event_data();
        io_data.interest = FLAG_READ;
        SocketRead {
            io_data: io_data,
            buf: buf,
            timeout: timeout,
            ret: None,
        }
    }

    #[inline]
    pub fn done(self) -> io::Result<usize> {
        loop {
            try!(co_io_result());
            match self.ret {
                // we already got the ret in subscribe
                Some(ret) => return ret,
                None => {}
            }

            // finish the read operaion
            match read(self.io_data.fd, self.buf).map_err(from_nix_error) {
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
                ret @ _ => return ret,
            }

            // the result is still WouldBlock, need to try again
            yield_with(&self);
        }
    }
}

impl<'a> EventSource for SocketRead<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        s.add_io_timer(&mut self.io_data, self.timeout);
        self.io_data.co.swap(co, Ordering::Release);
        // try the operation after set the co
        match read(self.io_data.fd, self.buf).map_err(from_nix_error) {
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => return,
            ret @ _ => self.ret = Some(ret),
        };

        // since we got data here, need to remove the timer handle and schedule
        self.io_data.schedule();
    }
}
