use std::io;
use std::time::Duration;
use std::sync::atomic::Ordering;
use super::super::from_nix_error;
use super::super::nix::unistd::write;
use super::super::{EventData, co_io_result};
use io::AsEventData;
use yield_now::yield_with;
use scheduler::get_scheduler;
use coroutine::{CoroutineImpl, EventSource};

pub struct SocketWrite<'a> {
    io_data: &'a mut EventData,
    buf: &'a [u8],
    timeout: Option<Duration>,
}

impl<'a> SocketWrite<'a> {
    pub fn new<T: AsEventData>(s: &'a T, buf: &'a [u8], timeout: Option<Duration>) -> Self {
        let io_data = s.as_event_data();
        SocketWrite {
            io_data: io_data,
            buf: buf,
            timeout: timeout,
        }
    }

    #[inline]
    pub fn done(self) -> io::Result<usize> {
        loop {
            try!(co_io_result());
            // clear the events
            self.io_data.io_flag.swap(0, Ordering::Relaxed);

            match write(self.io_data.fd, self.buf).map_err(from_nix_error) {
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
                ret @ _ => return ret,
            }

            // the result is still WouldBlock, need to try again
            yield_with(&self);
        }
    }
}

impl<'a> EventSource for SocketWrite<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        s.add_io_timer(&mut self.io_data, self.timeout);
        self.io_data.co.swap(co, Ordering::Release);

        // there is no event
        if self.io_data.io_flag.swap(0, Ordering::Relaxed) == 0 {
            return;
        }

        // since we got data here, need to remove the timer handle and schedule
        self.io_data.schedule();
    }
}
