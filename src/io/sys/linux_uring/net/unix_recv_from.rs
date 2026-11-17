use std::io;
use std::os::unix::net::SocketAddr;
use std::sync::Arc;
#[cfg(feature = "io_timeout")]
use std::time::Duration;

use io_uring::{opcode, types};

use super::super::event::{co_io_result, EventData};
#[cfg(feature = "io_cancel")]
use crate::coroutine_impl::co_cancel_data;
use crate::coroutine_impl::{is_coroutine, CoroutineImpl, EventSource};
#[cfg(feature = "io_cancel")]
use crate::io::sys::cancel::CancelIoData;
use crate::io::AsIoData;
use crate::os::unix::net::UnixDatagram;
use crate::scheduler::get_scheduler;
use crate::sync::delay_drop::DelayDrop;

/// Unix-datagram `recv_from`.
///
/// A std `SocketAddr` cannot be built from a raw `sockaddr_un` for unnamed
/// senders, so this adapter waits on a `MSG_PEEK` completion and then consumes
/// the datagram with the std `recv_from`, which returns the address. Only this
/// Unix-datagram path pays the extra syscall.
pub struct UnixRecvFrom<'a> {
    event_data: Arc<EventData>,
    buf: &'a mut [u8],
    socket: &'a std::os::unix::net::UnixDatagram,
    peek: [u8; 1],
    #[cfg(feature = "io_timeout")]
    timeout: Option<Duration>,
    can_drop: DelayDrop,
    pub(crate) is_coroutine: bool,
}

impl<'a> UnixRecvFrom<'a> {
    pub fn new(socket: &'a UnixDatagram, buf: &'a mut [u8]) -> Self {
        UnixRecvFrom {
            event_data: socket.0.as_io_data().new_request(),
            buf,
            socket: socket.0.inner(),
            peek: [0u8; 1],
            #[cfg(feature = "io_timeout")]
            timeout: socket.0.read_timeout().unwrap(),
            can_drop: DelayDrop::new(),
            is_coroutine: is_coroutine(),
        }
    }

    pub fn done(&mut self) -> io::Result<(usize, SocketAddr)> {
        co_io_result(&self.event_data, self.is_coroutine)?;
        self.socket.recv_from(self.buf)
    }
}

impl EventSource for UnixRecvFrom<'_> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        #[cfg(feature = "io_cancel")]
        let cancel = co_cancel_data(&co);
        let _g = self.can_drop.delay_drop();

        #[cfg(feature = "io_timeout")]
        if let Some(dur) = self.timeout {
            s.get_selector().add_io_timer(&self.event_data, dur);
        }

        let entry = opcode::Recv::new(
            types::Fd(self.event_data.fd()),
            self.peek.as_mut_ptr(),
            self.peek.len() as u32,
        )
        .flags(libc::MSG_PEEK)
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
