use std::io;
use std::time::Duration;
use std::sync::atomic::Ordering;
use io::AsEventData;
use yield_now::yield_with;
use scheduler::get_scheduler;
use coroutine::{CoroutineImpl, EventSource};
use super::super::nix::unistd::read;
use super::super::{EventData, from_nix_error, co_io_result};

pub struct SocketRead<'a> {
    io_data: &'a mut EventData,
    buf: &'a mut [u8],
    timeout: Option<Duration>,
}

impl<'a> SocketRead<'a> {
    pub fn new<T: AsEventData>(s: &'a T, buf: &'a mut [u8], timeout: Option<Duration>) -> Self {
        let io_data = s.as_event_data();
        SocketRead {
            io_data: io_data,
            buf: buf,
            timeout: timeout,
        }
    }

    #[inline]
    pub fn done(self) -> io::Result<usize> {
        loop {
            try!(co_io_result());

            // clear the io_flag
            self.io_data.io_flag.store(false, Ordering::Relaxed);

            // finish the read operaion
            match read(self.io_data.fd, self.buf).map_err(from_nix_error) {
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
                ret @ _ => return ret,
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

impl<'a> EventSource for SocketRead<'a> {
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
