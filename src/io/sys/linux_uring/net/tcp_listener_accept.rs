use std::io;
use std::net::SocketAddr;
use std::os::fd::AsRawFd;
use std::os::unix::io::FromRawFd;
use std::sync::Arc;

use io_uring::{opcode, types};

use super::super::event::{co_io_result, EventData};
#[cfg(feature = "io_cancel")]
use crate::coroutine_impl::co_cancel_data;
use crate::coroutine_impl::{is_coroutine, CoroutineImpl, EventSource};
#[cfg(feature = "io_cancel")]
use crate::io::sys::cancel::CancelIoData;
use crate::io::AsIoData;
use crate::net::{TcpListener, TcpStream};
use crate::scheduler::get_scheduler;
use crate::sync::delay_drop::DelayDrop;

pub struct TcpListenerAccept<'a> {
    event_data: Arc<EventData>,
    socket: &'a std::net::TcpListener,
    can_drop: DelayDrop,
    pub(crate) is_coroutine: bool,
}

impl<'a> TcpListenerAccept<'a> {
    pub fn new(socket: &'a TcpListener) -> io::Result<Self> {
        Ok(TcpListenerAccept {
            event_data: socket.as_io_data().new_request(),
            socket: socket.inner(),
            can_drop: DelayDrop::new(),
            is_coroutine: is_coroutine(),
        })
    }

    pub fn done(&mut self) -> io::Result<(TcpStream, SocketAddr)> {
        let fd = co_io_result(&self.event_data, self.is_coroutine)? as libc::c_int;
        // Accept returns the new socket; SOCK_NONBLOCK | SOCK_CLOEXEC were
        // passed with the Accept SQE.
        let stream = unsafe { std::net::TcpStream::from_raw_fd(fd) };
        let addr = stream.peer_addr()?;
        let io = super::super::add_socket(&stream)?;
        Ok((TcpStream::from_stream(stream, io), addr))
    }
}

impl EventSource for TcpListenerAccept<'_> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        #[cfg(feature = "io_cancel")]
        let cancel = co_cancel_data(&co);
        let _g = self.can_drop.delay_drop();

        let no_addr: *mut libc::sockaddr = std::ptr::null_mut();
        let no_len: *mut libc::socklen_t = std::ptr::null_mut();
        let entry = opcode::Accept::new(types::Fd(self.socket.as_raw_fd()), no_addr, no_len)
            .flags(libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC)
            .build();

        #[cfg(feature = "io_cancel")]
        let event_data = submit_co!(s, &self.event_data, co, entry);
        #[cfg(not(feature = "io_cancel"))]
        submit_co!(s, &self.event_data, co, entry);

        #[cfg(feature = "io_cancel")]
        {
            cancel.set_io(CancelIoData::new(&event_data));
            if cancel.is_canceled() {
                cancel.cancel();
            }
        }
    }
}
