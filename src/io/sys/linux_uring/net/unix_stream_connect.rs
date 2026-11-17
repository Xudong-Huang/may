use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::Arc;
#[cfg(feature = "io_timeout")]
use std::time::Duration;

use io_uring::{opcode, types};
use socket2::{Domain, SockAddr, Socket, Type};

use super::super::event::{co_io_result, EventData};
use super::super::{add_socket, IoData};
#[cfg(feature = "io_cancel")]
use crate::coroutine_impl::co_cancel_data;
use crate::coroutine_impl::{is_coroutine, CoroutineImpl, EventSource};
#[cfg(feature = "io_cancel")]
use crate::io::sys::cancel::CancelIoData;
use crate::io::OptionCell;
use crate::os::unix::net::UnixStream;
use crate::scheduler::get_scheduler;
use crate::sync::delay_drop::DelayDrop;

pub struct UnixStreamConnect {
    event_data: Arc<EventData>,
    io_data: OptionCell<IoData>,
    stream: OptionCell<Socket>,
    addr: SockAddr,
    can_drop: DelayDrop,
    pub(crate) is_coroutine: bool,
}

impl UnixStreamConnect {
    pub fn new<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let addr = SockAddr::unix(path)?;
        let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
        socket.set_nonblocking(true)?;
        let io = add_socket(&socket)?;
        let event_data = io.new_request();

        Ok(UnixStreamConnect {
            event_data,
            io_data: OptionCell::new(io),
            stream: OptionCell::new(socket),
            addr,
            can_drop: DelayDrop::new(),
            is_coroutine: is_coroutine(),
        })
    }

    /// The io_uring backend submits `Connect`; the socket must not already be
    /// connecting.
    #[inline]
    pub fn check_connected(&mut self) -> io::Result<bool> {
        Ok(false)
    }

    pub fn done(&mut self) -> io::Result<UnixStream> {
        co_io_result(&self.event_data, self.is_coroutine)?;
        let stream: std::os::unix::net::UnixStream = self.stream.take().into();
        Ok(UnixStream::from_coio(crate::io::CoIo::from_raw(
            stream,
            self.io_data.take(),
        )))
    }
}

impl EventSource for UnixStreamConnect {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        #[cfg(feature = "io_cancel")]
        let cancel = co_cancel_data(&co);
        let _g = self.can_drop.delay_drop();

        // Preserve the readiness backend's 2-second guard for Unix connect.
        #[cfg(feature = "io_timeout")]
        s.get_selector()
            .add_io_timer(&self.event_data, Duration::from_secs(2));

        let entry = opcode::Connect::new(
            types::Fd(self.stream.as_raw_fd()),
            self.addr.as_ptr() as *const libc::sockaddr,
            self.addr.len(),
        )
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
