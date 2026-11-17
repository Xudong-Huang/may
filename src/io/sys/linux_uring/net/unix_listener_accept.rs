use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::io::FromRawFd;
use std::os::unix::net::{self, SocketAddr};
use std::sync::Arc;

use io_uring::{opcode, types};

use super::super::event::{co_io_result, EventData};
#[cfg(feature = "io_cancel")]
use crate::coroutine_impl::co_cancel_data;
use crate::coroutine_impl::{is_coroutine, CoroutineImpl, EventSource};
#[cfg(feature = "io_cancel")]
use crate::io::sys::cancel::CancelIoData;
use crate::io::AsIoData;
use crate::os::unix::net::{UnixListener, UnixStream};
use crate::scheduler::get_scheduler;
use crate::sync::delay_drop::DelayDrop;

pub struct UnixListenerAccept<'a> {
    event_data: Arc<EventData>,
    socket: &'a net::UnixListener,
    can_drop: DelayDrop,
    pub(crate) is_coroutine: bool,
}

impl<'a> UnixListenerAccept<'a> {
    pub fn new(socket: &'a UnixListener) -> io::Result<Self> {
        Ok(UnixListenerAccept {
            event_data: socket.0.as_io_data().new_request(),
            socket: socket.0.inner(),
            can_drop: DelayDrop::new(),
            is_coroutine: is_coroutine(),
        })
    }

    pub fn done(&mut self) -> io::Result<(UnixStream, SocketAddr)> {
        let fd = co_io_result(&self.event_data, self.is_coroutine)? as libc::c_int;
        let stream = unsafe { net::UnixStream::from_raw_fd(fd) };
        stream.set_nonblocking(true)?;
        let addr = stream.peer_addr()?;
        let io = crate::io::CoIo::new(stream)?;
        Ok((UnixStream::from_coio(io), addr))
    }
}

impl EventSource for UnixListenerAccept<'_> {
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
