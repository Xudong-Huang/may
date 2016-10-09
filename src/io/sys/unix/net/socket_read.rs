use std::io;
use std::time::Duration;
use std::os::unix::io::RawFd;
use super::co_io_result;
use super::super::from_nix_error;
use super::super::nix::unistd::read;
use super::super::{EventData, FLAG_READ};
use yield_now::yield_with;
use scheduler::get_scheduler;
use coroutine::{CoroutineImpl, EventSource};


pub struct SocketRead<'a> {
    io_data: EventData,
    buf: &'a mut [u8],
    timeout: Option<Duration>,
}

impl<'a> SocketRead<'a> {
    pub fn new(socket: RawFd, buf: &'a mut [u8], timeout: Option<Duration>) -> Self {
        SocketRead {
            io_data: EventData::new(socket, FLAG_READ),
            buf: buf,
            timeout: timeout,
        }
    }

    #[inline]
    pub fn done(self) -> io::Result<usize> {
        loop {
            try!(co_io_result());
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
        // prepare the co first
        self.io_data.co = Some(co);

        // register the io operaton
        co_try!(s,
                self.io_data.co.take().unwrap(),
                s.add_io(&mut self.io_data, self.timeout));
    }
}
