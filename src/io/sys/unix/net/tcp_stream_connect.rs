use std::{self, io};
use std::time::Duration;
use std::sync::atomic::Ordering;
use std::net::{ToSocketAddrs, SocketAddr};
use std::os::unix::io::{FromRawFd, IntoRawFd, AsRawFd};
use net::TcpStream;
use net2::TcpBuilder;
use yield_now::yield_with;
use scheduler::get_scheduler;
use coroutine::{CoroutineImpl, EventSource};
use super::super::{libc, IoData, co_io_result, add_socket};

pub struct TcpStreamConnect {
    io_data: IoData,
    builder: TcpBuilder,
    addr: SocketAddr,
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

                add_socket(&builder).map(|io| {
                    TcpStreamConnect {
                        io_data: io,
                        builder: builder,
                        addr: addr,
                    }
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
            self.io_data.inner().io_flag.store(false, Ordering::Relaxed);

            match self.builder.connect(&self.addr) {
                Err(ref e) if e.raw_os_error() == Some(libc::EINPROGRESS) => {}
                Err(ref e) if e.raw_os_error() == Some(libc::EALREADY) => {}
                ret => return ret.map(|s| TcpStream::from_stream(s, self.io_data.clone())),
            }

            if self.io_data.inner().io_flag.swap(false, Ordering::Relaxed) {
                continue;
            }

            // the result is still EINPROGRESS, need to try again
            yield_with(&self);
        }
    }
}

impl EventSource for TcpStreamConnect {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let io_data = self.io_data.inner();
        get_scheduler().get_selector().add_io_timer(io_data, Some(Duration::from_secs(10)));
        io_data.co.swap(co, Ordering::Release);

        // there is no event
        if !io_data.io_flag.load(Ordering::Relaxed) {
            return;
        }

        // since we got data here, need to remove the timer handle and schedule
        io_data.schedule();
    }
}
