use std::io;
use std::ops::Deref;
use std::time::Duration;
use std::sync::atomic::Ordering;
use io::AsIoData;
use nix::unistd::read;
use yield_now::yield_with;
use scheduler::get_scheduler;
use sync::delay_drop::DelayDrop;
use super::super::{co_io_result, from_nix_error, IoData};
use coroutine_impl::{co_cancel_data, CoroutineImpl, EventSource};

pub struct SocketRead<'a> {
    io_data: &'a IoData,
    buf: &'a mut [u8],
    timeout: Option<Duration>,
    can_drop: DelayDrop,
}

impl<'a> SocketRead<'a> {
    pub fn new<T: AsIoData>(s: &'a T, buf: &'a mut [u8], timeout: Option<Duration>) -> Self {
        SocketRead {
            io_data: s.as_io_data(),
            buf: buf,
            timeout: timeout,
            can_drop: DelayDrop::new(),
        }
    }

    #[inline]
    pub fn done(self) -> io::Result<usize> {
        loop {
            try!(co_io_result());

            // clear the io_flag
            self.io_data.io_flag.store(false, Ordering::Relaxed);

            // finish the read operation
            match read(self.io_data.fd, self.buf).map_err(from_nix_error) {
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

impl<'a> EventSource for SocketRead<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        // when exit the scope the `can_drop` will be set to true
        let _g = self.can_drop.delay_drop();

        let cancel = co_cancel_data(&co);
        get_scheduler()
            .get_selector()
            .add_io_timer(self.io_data, self.timeout);
        // after register the coroutine, it's possible that other thread run it immediately
        // and cause the process after it invalid, this is kind of user and kernel competition
        // so we need to delay the drop of the EventSource, that's why _g is here
        self.io_data.co.swap(co, Ordering::Release);

        // there is event, re-run the coroutine
        if self.io_data.io_flag.load(Ordering::Relaxed) {
            return self.io_data.schedule();
        }

        // register the cancel io data
        cancel.set_io(self.io_data.deref().clone());
        // re-check the cancel status
        if cancel.is_canceled() {
            unsafe { cancel.cancel() };
        }
    }
}
