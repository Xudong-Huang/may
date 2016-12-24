use std::io;
use std::ops::Deref;
use std::time::Duration;
use std::sync::atomic::Ordering;
use super::super::nix::unistd::read;
use super::super::{IoData, from_nix_error, co_io_result};
use io::AsIoData;
use cancel::Cancel;
use yield_now::yield_with;
use io::cancel::CancelIoImpl;
use scheduler::get_scheduler;
use coroutine::{CoroutineImpl, EventSource, get_cancel_data};

pub struct SocketRead<'a> {
    io_data: &'a IoData,
    buf: &'a mut [u8],
    timeout: Option<Duration>,
    io_cancel: &'static Cancel<CancelIoImpl>,
}

impl<'a> SocketRead<'a> {
    pub fn new<T: AsIoData>(s: &'a T, buf: &'a mut [u8], timeout: Option<Duration>) -> Self {
        SocketRead {
            io_data: s.as_io_data(),
            buf: buf,
            timeout: timeout,
            io_cancel: get_cancel_data(),
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

impl<'a> EventSource for SocketRead<'a> {
    fn get_cancel_data(&self) -> Option<&Cancel<CancelIoImpl>> {
        Some(self.io_cancel)
    }

    fn subscribe(&mut self, co: CoroutineImpl) {
        get_scheduler().get_selector().add_io_timer(self.io_data, self.timeout);
        self.io_data.co.swap(co, Ordering::Release);

        // there is event, re-run the coroutine
        if self.io_data.io_flag.load(Ordering::Relaxed) {
            return self.io_data.schedule();
        }

        // deal with the cancel
        self.get_cancel_data().map(|cancel| {
            // register the cancel io data
            cancel.set_io(self.io_data.deref().clone());
            // re-check the cancel status
            if cancel.is_canceled() {
                unsafe { cancel.cancel() };
            }
        });
    }
}
