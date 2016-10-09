use std::io;
use std::time::Duration;
use std::os::unix::io::AsRawFd;
use std::net::{ToSocketAddrs, SocketAddr};
use super::co_io_result;
use super::super::libc;
use super::super::{EventData, FLAG_WRITE};
use net::TcpStream;
use net2::TcpBuilder;
use yield_now::yield_with;
use scheduler::get_scheduler;
use coroutine::{CoroutineImpl, EventSource};

pub struct TcpStreamConnect {
    io_data: EventData,
    stream: TcpBuilder,
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
            .map(|(builder, addr)| {
                TcpStreamConnect {
                    io_data: EventData::new(builder.as_raw_fd(), FLAG_WRITE),
                    addr: addr,
                    stream: builder,
                }
            })
    }

    #[inline]
    pub fn done(self) -> io::Result<TcpStream> {
        loop {
            try!(co_io_result());
            match self.stream.connect(&self.addr) {
                Err(ref e) if e.raw_os_error() == Some(libc::EINPROGRESS) => {}
                Err(e) => return Err(e),
                Ok(..) => return self.stream.to_tcp_stream().and_then(|s| TcpStream::new(s)),
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
