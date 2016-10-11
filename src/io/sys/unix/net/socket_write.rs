use std::io;
use std::time::Duration;
use std::os::unix::io::RawFd;
use super::co_io_result;
use super::super::from_nix_error;
use super::super::nix::unistd::write;
use super::super::{EventData, FLAG_WRITE};
use yield_now::yield_with;
use scheduler::get_scheduler;
use coroutine::{CoroutineImpl, EventSource};

pub struct SocketWrite<'a> {
    io_data: EventData,
    buf: &'a [u8],
    timeout: Option<Duration>,
}

impl<'a> SocketWrite<'a> {
    pub fn new(socket: RawFd, buf: &'a [u8], timeout: Option<Duration>) -> Self {
        SocketWrite {
            io_data: EventData::new(socket, FLAG_WRITE),
            buf: buf,
            timeout: timeout,
        }
    }

    #[inline]
    pub fn done(self) -> io::Result<usize> {
        loop {
            try!(co_io_result());
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
        self.io_data.co = Some(co);
    }
}
