use std::io;
use std::sync::atomic::Ordering;

use super::super::{co_io_result, from_nix_error, IoData};
use coroutine_impl::{CoroutineImpl, EventSource};
use io::AsIoData;
use nix::unistd::write;
use sync::delay_drop::DelayDrop;
use yield_now::yield_with;

pub struct FileWrite<'a> {
    io_data: &'a IoData,
    buf: &'a [u8],
    can_drop: DelayDrop,
}

impl<'a> FileWrite<'a> {
    pub fn new<T: AsIoData>(s: &'a T, offset: u64, buf: &'a [u8]) -> Self {
        FileWrite {
            io_data: s.as_io_data(),
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

            match write(self.io_data.fd, self.buf).map_err(from_nix_error) {
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
