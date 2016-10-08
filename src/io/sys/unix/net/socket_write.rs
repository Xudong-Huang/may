use std::{self, io};
use std::io::Read;
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
                Err(err) => {
                    if err.kind() != io::ErrorKind::WouldBlock {
                        return Err(err);
                    }
                }
                Ok(size) => {
                    return Ok(size);
                }
            }

            // the result is still WouldBlock, need to try again
            yield_with(&self);
        }
    }
}

impl<'a> EventSource for SocketWrite<'a> {
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