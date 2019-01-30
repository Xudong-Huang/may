use std::io;
use std::ops::Deref;
use std::os::unix::net::{self, SocketAddr};
use std::sync::atomic::Ordering;

use coroutine_impl::{co_cancel_data, CoroutineImpl, EventSource};
use io::sys::{co_io_result, IoData};
use io::{AsIoData, CoIo};
use os::unix::net::{UnixListener, UnixStream};
use sync::delay_drop::DelayDrop;
use yield_now::yield_with;

pub struct UnixListenerAccept<'a> {
    io_data: &'a IoData,
    socket: &'a net::UnixListener,
    can_drop: DelayDrop,
}

impl<'a> UnixListenerAccept<'a> {
    pub fn new(socket: &'a UnixListener) -> io::Result<Self> {
        Ok(UnixListenerAccept {
            io_data: socket.0.as_io_data(),
            socket: socket.0.inner(),
            can_drop: DelayDrop::new(),
        })
    }

    #[inline]
    pub fn done(self) -> io::Result<(UnixStream, SocketAddr)> {
        loop {
            co_io_result()?;

            // clear the io_flag
            self.io_data.io_flag.store(false, Ordering::Relaxed);

            match self.socket.accept() {
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e),
                Ok((s, a)) => {
                    let s = UnixStream::from_coio(CoIo::new(s)?);
                    return Ok((s, a));
                }
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

impl<'a> EventSource for UnixListenerAccept<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let _g = self.can_drop.delay_drop();
        let cancel = co_cancel_data(&co);
        // if there is no timer we don't need to call add_io_timer
        self.io_data.co.swap(co, Ordering::Release);

        // there is event happened
        if self.io_data.io_flag.load(Ordering::Acquire) {
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
