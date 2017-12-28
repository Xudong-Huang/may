use std::{self, io};
use std::ops::Deref;
use std::time::Duration;
use std::sync::atomic::Ordering;
use std::net::{SocketAddr, ToSocketAddrs};
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd};
use super::super::{add_socket, co_io_result, libc, IoData};
use net::TcpStream;
use net2::TcpBuilder;
use yield_now::yield_with;
use scheduler::get_scheduler;
use sync::delay_drop::DelayDrop;
use coroutine_impl::{co_cancel_data, CoroutineImpl, EventSource};

pub struct TcpStreamConnect {
    io_data: IoData,
    builder: TcpBuilder,
    addr: SocketAddr,
    can_drop: DelayDrop,
}

impl TcpStreamConnect {
    pub fn new<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let err = io::Error::new(io::ErrorKind::Other, "no socket addresses resolved");
        try!(addr.to_socket_addrs())
            .fold(Err(err), |prev, addr| {
                prev.or_else(|_| {
                    let builder = match addr {
                        SocketAddr::V4(..) => try!(TcpBuilder::new_v4()),
                        SocketAddr::V6(..) => try!(TcpBuilder::new_v6()),
                    };
                    Ok((builder, addr))
                })
            })
            .and_then(|(builder, addr)| {
                // before yield we must set the socket to nonblocking mode and registe to selector
                let fd = builder.as_raw_fd();
                let s: std::net::TcpStream = unsafe { FromRawFd::from_raw_fd(fd) };
                try!(s.set_nonblocking(true));
                // prevent close the socket
                s.into_raw_fd();

                add_socket(&builder).map(|io| TcpStreamConnect {
                    io_data: io,
                    builder: builder,
                    addr: addr,
                    can_drop: DelayDrop::new(),
                })
            })
    }

    #[inline]
    pub fn get_stream(&mut self) -> Option<io::Result<TcpStream>> {
        // unix connect is some like completion mode
        // we must give the connect request first to the system
        match self.builder.connect(&self.addr) {
            Err(ref e) if e.raw_os_error() == Some(libc::EINPROGRESS) => None,
            ret => Some(ret.map(|s| TcpStream::from_stream(s, self.io_data.clone()))),
        }
    }

    #[inline]
    pub fn done(self) -> io::Result<TcpStream> {
        loop {
            try!(co_io_result());

            // clear the io_flag
            self.io_data.io_flag.store(false, Ordering::Relaxed);

            match self.builder.connect(&self.addr) {
                Err(ref e) if e.raw_os_error() == Some(libc::EINPROGRESS) => {}
                Err(ref e) if e.raw_os_error() == Some(libc::EALREADY) => {}
                Err(ref e) if e.raw_os_error() == Some(libc::EISCONN) => {
                    return self.builder
                        .to_tcp_stream()
                        .map(|s| TcpStream::from_stream(s, self.io_data.clone()))
                }
                ret => return ret.map(|s| TcpStream::from_stream(s, self.io_data.clone())),
            }

            if self.io_data.io_flag.swap(false, Ordering::Relaxed) {
                continue;
            }

            // the result is still EINPROGRESS, need to try again
            self.can_drop.reset();
            yield_with(&self);
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
            .add_io_timer(io_data, Some(Duration::from_secs(10)));
        io_data.co.swap(co, Ordering::Release);

        // there is event, re-run the coroutine
        if io_data.io_flag.load(Ordering::Relaxed) {
            return io_data.schedule();
        }

        // register the cancel io data
        cancel.set_io(self.io_data.deref().clone());
        // re-check the cancel status
        if cancel.is_canceled() {
            unsafe { cancel.cancel() };
        }
    }
}
