use std::{self, io};
use std::time::Duration;
use std::net::{ToSocketAddrs, SocketAddr};
use std::os::unix::io::{FromRawFd, IntoRawFd, AsRawFd};
use super::co_io_result;
use super::super::libc;
use super::super::{EventData, FLAG_WRITE};
use net::TcpStream;
use net2::TcpBuilder;
use io::net::add_socket;
use yield_now::yield_with;
use scheduler::get_scheduler;
use coroutine::{CoroutineImpl, EventSource};

pub struct TcpStreamConnect {
    io_data: EventData,
    builder: TcpBuilder,
    stream: Option<TcpStream>,
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

                add_socket(&builder).and_then(|_| {
                    let mut me = TcpStreamConnect {
                        io_data: EventData::new(builder.as_raw_fd(), FLAG_WRITE),
                        builder: builder,
                        stream: None,
                        addr: addr,
                    };
                    // unix connect is some like completion mode
                    // we must give the connect request first to the system
                    match me.builder.connect(&me.addr) {
                        Err(ref e) if e.raw_os_error() == Some(libc::EINPROGRESS) => {}
                        Err(e) => return Err(e),
                        Ok(s) => me.stream = Some(TcpStream::from_stream(s)),
                    }
                    Ok(me)
                })
            })
    }

    #[inline]
    pub fn done(mut self) -> io::Result<TcpStream> {
        if self.stream.is_some() {
            // we already got the stream in new function
            return Ok(self.stream.take().unwrap());
        }

        loop {
            try!(co_io_result());
            match self.builder.connect(&self.addr) {
                Err(ref e) if e.raw_os_error() == Some(libc::EINPROGRESS) => {}
                Err(e) => return Err(e),
                Ok(s) => return Ok(TcpStream::from_stream(s)),
            }
            // the result is still EINPROGRESS, need to try again
            yield_with(&self);
        }
    }
}

impl EventSource for TcpStreamConnect {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        // prepare the co first
        self.io_data.co = Some(co);

        // register the io operaton, by default the timeout is 10 sec
        co_try!(s,
                self.io_data.co.take().unwrap(),
                s.add_io(&mut self.io_data, Some(Duration::from_secs(10))));
    }
}
