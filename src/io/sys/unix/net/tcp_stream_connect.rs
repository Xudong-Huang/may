use std::{self, io};
// use std::time::Duration;
use std::net::{ToSocketAddrs, SocketAddr};
use std::os::unix::io::{FromRawFd, IntoRawFd, AsRawFd};
use super::super::{libc, EventData, FLAG_WRITE, co_io_result, add_socket};
use net::TcpStream;
use net2::TcpBuilder;
use yield_now::yield_with;
use scheduler::get_scheduler;
use coroutine::{CoroutineImpl, EventSource};

pub struct TcpStreamConnect {
    io_data: EventData,
    builder: TcpBuilder,
    addr: SocketAddr,
    ret: Option<io::Result<TcpStream>>,
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

                add_socket(&builder).map(|_| {
                    // unix connect is some like completion mode
                    // we must give the connect request first to the system
                    let ret = match builder.connect(&addr) {
                        Err(ref e) if e.raw_os_error() == Some(libc::EINPROGRESS) => None,
                        ret @ _ => Some(ret.map(|s| TcpStream::from_stream(s))),
                    };

                    TcpStreamConnect {
                        io_data: EventData::new(builder.as_raw_fd(), FLAG_WRITE),
                        builder: builder,
                        addr: addr,
                        ret: ret,
                    }
                })
            })
    }

    #[inline]
    pub fn get_stream(&mut self) -> Option<io::Result<TcpStream>> {
        self.ret.take()
    }

    #[inline]
    pub fn done(self) -> io::Result<TcpStream> {
        let s = get_scheduler().get_selector();
        match self.ret {
            Some(s) => return s,
            None => {}
        }

        loop {
            s.del_fd(self.io_data.fd);
            try!(co_io_result());

            match self.builder.connect(&self.addr) {
                Err(ref e) if e.raw_os_error() == Some(libc::EINPROGRESS) => {}
                Err(ref e) if e.raw_os_error() == Some(libc::EALREADY) => {}
                ret @ _ => return ret.map(|s| TcpStream::from_stream(s)),
            }

            // the result is still EINPROGRESS, need to try again
            yield_with(&self);
        }
    }
}

impl EventSource for TcpStreamConnect {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        // s.add_io_timer(&mut self.io_data, Some(Duration::from_secs(10)));
        s.add_io_timer(&mut self.io_data, None);
        self.io_data.co = Some(co);

        // register the io operaton
        co_try!(s,
                self.io_data.co.take().expect("can't get co"),
                s.get_selector().add_io(&self.io_data));
    }
}
