use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::{self, io};

use super::super::{co_io_result, IoData};
use crate::coroutine_impl::{CoroutineImpl, EventSource};
use crate::io::AsIoData;
use crate::os::unix::net::UnixDatagram;
use crate::scheduler::get_scheduler;
use crate::sync::delay_drop::DelayDrop;
use crate::yield_now::yield_with;

pub struct UnixSendTo<'a> {
    io_data: &'a IoData,
    buf: &'a [u8],
    socket: &'a std::os::unix::net::UnixDatagram,
    path: &'a Path,
    timeout: Option<Duration>,
    can_drop: DelayDrop,
}

impl<'a> UnixSendTo<'a> {
    pub fn new(socket: &'a UnixDatagram, buf: &'a [u8], path: &'a Path) -> io::Result<Self> {
        Ok(UnixSendTo {
            io_data: socket.0.as_io_data(),
            buf,
            socket: socket.0.inner(),
            path,
            timeout: socket.write_timeout().unwrap(),
            can_drop: DelayDrop::new(),
        })
    }

    #[inline]
    pub fn done(self) -> io::Result<usize> {
        loop {
            co_io_result()?;

            // clear the io_flag
            self.io_data.io_flag.store(false, Ordering::Relaxed);

            match self.socket.send_to(self.buf, self.path) {
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

impl<'a> EventSource for UnixSendTo<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let _g = self.can_drop.delay_drop();
        get_scheduler()
            .get_selector()
            .add_io_timer(self.io_data, self.timeout);
        self.io_data.co.swap(co, Ordering::Release);

        // there is event, re-run the coroutine
        if self.io_data.io_flag.load(Ordering::Acquire) {
            self.io_data.schedule();
        }
    }
}
