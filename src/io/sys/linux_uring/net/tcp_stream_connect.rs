use std::io;
use std::net::{SocketAddr, ToSocketAddrs};
use std::os::fd::AsRawFd;
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
use crate::net::TcpStream;
use crate::scheduler::get_scheduler;
use crate::sync::delay_drop::DelayDrop;

pub struct TcpStreamConnect {
    event_data: Arc<EventData>,
    io_data: OptionCell<IoData>,
    stream: OptionCell<Socket>,
    addr: SockAddr,
    #[cfg(feature = "io_timeout")]
    timeout: Option<Duration>,
    can_drop: DelayDrop,
    pub(crate) is_coroutine: bool,
}

impl TcpStreamConnect {
    pub fn new<A: ToSocketAddrs>(
        addr: A,
        #[cfg(feature = "io_timeout")] timeout: Option<Duration>,
    ) -> io::Result<Self> {
        let addr: SocketAddr = addr
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::other("no socket addresses resolved"))?;

        let socket = match addr {
            SocketAddr::V4(..) => Socket::new(Domain::IPV4, Type::STREAM, None)?,
            SocketAddr::V6(..) => Socket::new(Domain::IPV6, Type::STREAM, None)?,
        };
        socket.set_nonblocking(true)?;
        let io = add_socket(&socket)?;
        let event_data = io.new_request();

        Ok(TcpStreamConnect {
            event_data,
            io_data: OptionCell::new(io),
            stream: OptionCell::new(socket),
            addr: addr.into(),
            #[cfg(feature = "io_timeout")]
            timeout,
            can_drop: DelayDrop::new(),
            is_coroutine: is_coroutine(),
        })
    }

    /// The io_uring backend submits `Connect`; it must not call `connect(2)`
    /// first, or the SQE would see a socket already in progress.
    #[inline]
    pub fn check_connected(&mut self) -> io::Result<bool> {
        Ok(false)
    }

    pub fn done(&mut self) -> io::Result<TcpStream> {
        co_io_result(&self.event_data, self.is_coroutine)?;
        let stream: std::net::TcpStream = self.stream.take().into();
        Ok(TcpStream::from_stream(stream, self.io_data.take()))
    }
}

impl EventSource for TcpStreamConnect {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        #[cfg(feature = "io_cancel")]
        let cancel = co_cancel_data(&co);
        let _g = self.can_drop.delay_drop();

        #[cfg(feature = "io_timeout")]
        if let Some(dur) = self.timeout {
            s.get_selector().add_io_timer(&self.event_data, dur);
        }

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
