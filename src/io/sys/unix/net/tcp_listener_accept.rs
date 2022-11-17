use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::{self, io};

use super::super::{add_socket, co_io_result, IoData};
use crate::coroutine_impl::{co_get_handle, is_coroutine, CoroutineImpl, EventSource};
use crate::io::AsIoData;
use crate::net::{TcpListener, TcpStream};
use crate::yield_now::yield_with_io;

pub struct TcpListenerAccept<'a> {
    io_data: &'a IoData,
    socket: &'a std::net::TcpListener,
    is_coroutine: bool,
}

impl<'a> TcpListenerAccept<'a> {
    pub fn new(socket: &'a TcpListener) -> io::Result<Self> {
        Ok(TcpListenerAccept {
            io_data: socket.as_io_data(),
            socket: socket.inner(),
            is_coroutine: is_coroutine(),
        })
    }

    pub fn done(&mut self) -> io::Result<(TcpStream, SocketAddr)> {
        loop {
            co_io_result(self.is_coroutine)?;

            // clear the io_flag
            self.io_data.io_flag.store(false, Ordering::Relaxed);

            match self.socket.accept() {
                Ok((s, a)) => {
                    s.set_nonblocking(true)?;
                    return add_socket(&s).map(|io| (TcpStream::from_stream(s, io), a));
                }
                Err(e) => {
                    // raw_os_error is faster than kind
                    let raw_err = e.raw_os_error();
                    if raw_err == Some(libc::EAGAIN) || raw_err == Some(libc::EWOULDBLOCK) {
                        // do nothing here
                    } else {
                        return Err(e);
                    }
                }
            }

            if self.io_data.io_flag.swap(false, Ordering::Relaxed) {
                continue;
            }

            // the result is still WouldBlock, need to try again
            yield_with_io(self, self.is_coroutine);
        }
    }
}

impl<'a> EventSource for TcpListenerAccept<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let handle = co_get_handle(&co);
        let cancel = handle.get_cancel();
        let io_data = (*self.io_data).clone();
        // if there is no timer we don't need to call add_io_timer
        self.io_data.co.swap(co, Ordering::Release);

        // there is event happened
        if io_data.io_flag.load(Ordering::Acquire) {
            return io_data.schedule();
        }

        // register the cancel io data
        cancel.set_io(io_data);
        // re-check the cancel status
        if cancel.is_canceled() {
            unsafe { cancel.cancel() };
        }
    }
}
