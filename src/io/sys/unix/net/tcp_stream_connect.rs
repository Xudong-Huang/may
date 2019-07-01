use std::io;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::atomic::Ordering;
use std::time::Duration;

use super::super::{add_socket, co_io_result, IoData};
use crate::coroutine_impl::{co_cancel_data, CoroutineImpl, EventSource};
use crate::io::OptionCell;
use crate::net::TcpStream;
use crate::scheduler::get_scheduler;
use crate::sync::delay_drop::DelayDrop;
use crate::yield_now::yield_with;
use libc;
use socket2::Socket;

pub struct TcpStreamConnect {
    io_data: OptionCell<IoData>,
    stream: OptionCell<Socket>,
    timeout: Option<Duration>,
    addr: SocketAddr,
    can_drop: DelayDrop,
    is_connected: bool,
}

impl TcpStreamConnect {
    pub fn new<A: ToSocketAddrs>(addr: A, timeout: Option<Duration>) -> io::Result<Self> {
        use socket2::{Domain, Type};

        let err = io::Error::new(io::ErrorKind::Other, "no socket addresses resolved");
        addr.to_socket_addrs()?
            .fold(Err(err), |prev, addr| {
                prev.or_else(|_| {
                    let stream = match addr {
                        SocketAddr::V4(..) => Socket::new(Domain::ipv4(), Type::stream(), None)?,
                        SocketAddr::V6(..) => Socket::new(Domain::ipv4(), Type::stream(), None)?,
                    };
                    Ok((stream, addr))
                })
            })
            .and_then(|(stream, addr)| {
                // before yield we must set the socket to nonblocking mode and registe to selector
                stream.set_nonblocking(true)?;

                add_socket(&stream).map(|io| TcpStreamConnect {
                    io_data: OptionCell::new(io),
                    stream: OptionCell::new(stream),
                    timeout,
                    addr,
                    can_drop: DelayDrop::new(),
                    is_connected: false,
                })
            })
    }

    #[inline]
    // return ture if it's connected
    pub fn check_connected(&mut self) -> io::Result<bool> {
        // unix connect is some like completion mode
        // we must give the connect request first to the system
        match self.stream.connect(&self.addr.into()) {
            Ok(_) => {
                self.is_connected = true;
                Ok(true)
            }
            Err(ref e) if e.raw_os_error() == Some(libc::EINPROGRESS) => Ok(false),
            Err(e) => Err(e),
        }
    }

    pub fn done(&mut self) -> io::Result<TcpStream> {
        fn convert_to_stream(s: &mut TcpStreamConnect) -> TcpStream {
            let stream = s.stream.take().into_tcp_stream();
            TcpStream::from_stream(stream, s.io_data.take())
        }

        // first check if it's already connected
        if self.is_connected {
            return Ok(convert_to_stream(self));
        }

        loop {
            co_io_result()?;

            // clear the io_flag
            self.io_data.io_flag.store(false, Ordering::Relaxed);

            match self.stream.connect(&self.addr.into()) {
                Ok(_) => return Ok(convert_to_stream(self)),
                Err(ref e) if e.raw_os_error() == Some(libc::EINPROGRESS) => {}
                Err(ref e) if e.raw_os_error() == Some(libc::EALREADY) => {}
                Err(ref e) if e.raw_os_error() == Some(libc::EISCONN) => {
                    return Ok(convert_to_stream(self));
                }
                Err(e) => return Err(e),
            }

            if self.io_data.io_flag.swap(false, Ordering::Relaxed) {
                continue;
            }

            // the result is still EINPROGRESS, need to try again
            self.can_drop.reset();
            yield_with(self);
        }
    }
}

impl EventSource for TcpStreamConnect {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let _g = self.can_drop.delay_drop();
        let cancel = co_cancel_data(&co);
        let io_data = &self.io_data;
        get_scheduler()
            .get_selector()
            .add_io_timer(io_data, self.timeout);
        io_data.co.swap(co, Ordering::Release);

        // there is event, re-run the coroutine
        if io_data.io_flag.load(Ordering::Acquire) {
            return io_data.schedule();
        }

        // register the cancel io data
        cancel.set_io(self.io_data.clone());
        // re-check the cancel status
        if cancel.is_canceled() {
            unsafe { cancel.cancel() };
        }
    }
}
