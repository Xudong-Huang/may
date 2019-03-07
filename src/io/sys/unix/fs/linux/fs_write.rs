use std::io;
use std::os::unix::io::{AsRawFd, RawFd};
use std::sync::atomic::Ordering;

use super::{AsFileIo, FileIo};
use coroutine_impl::{CoroutineImpl, EventSource};
use io::sys::{co_io_result, from_nix_error};
use nix::unistd::write;
use sync::delay_drop::DelayDrop;
use yield_now::yield_with;

pub struct FileWrite<'a> {
    io_data: &'a FileIo,
    buf: &'a [u8],
    file: RawFd,
    can_drop: DelayDrop,
}

impl<'a> FileWrite<'a> {
    pub fn new<T: AsFileIo + AsRawFd>(s: &'a T, _offset: u64, buf: &'a [u8]) -> Self {
        FileWrite {
            io_data: s.as_file_io(),
            file: s.as_raw_fd(),
            buf,
            can_drop: DelayDrop::new(),
        }
    }

    #[inline]
    pub fn done(self) -> io::Result<usize> {
        loop {
            co_io_result()?;

            // clear the io_flag
            self.io_data.io_flag.store(false, Ordering::Relaxed);

            match write(self.io_data.fd.as_raw_fd(), self.buf).map_err(from_nix_error) {
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
                ret => return ret,
            }

            if self.io_data.io_flag.swap(false, Ordering::Relaxed) {
                continue;
            }

            // the result is still WouldBlock, need to try again
            self.can_drop.reset();
            yield_with(&self);
        }
    }
}

impl<'a> EventSource for FileWrite<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let _g = self.can_drop.delay_drop();
        self.io_data.co.swap(co, Ordering::Release);

        // there is event, re-run the coroutine
        if self.io_data.io_flag.load(Ordering::Acquire) {
            self.io_data.schedule();
        }
    }
}
