use std::io;
use std::ops::Deref;
use std::time::Duration;
use std::sync::atomic::Ordering;
use std::net::{SocketAddr, ToSocketAddrs};
use libc;
use net::TcpStream;
use socket2::Socket;
use yield_now::yield_with;
use scheduler::get_scheduler;
use sync::delay_drop::DelayDrop;
use super::super::{add_socket, co_io_result, IoData};
use coroutine_impl::{co_cancel_data, CoroutineImpl, EventSource};

pub struct TcpStreamConnect {
    io_data: IoData,
    stream: Option<Socket>,
    addr: SocketAddr,
    can_drop: DelayDrop,
}

impl TcpStreamConnect {
    pub fn new<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
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
                    io_data: io,
                    stream: Some(stream),
                    addr: addr,
                    can_drop: DelayDrop::new(),
                })
            })
    }

    #[inline]
    pub fn get_stream(&mut self) -> Option<io::Result<TcpStream>> {
        // unix connect is some like completion mode
        // we must give the connect request first to the system
        assert_eq!(self.stream.is_none(), false);
        match self.stream.as_ref().unwrap().connect(&self.addr.into()) {
            Err(ref e) if e.raw_os_error() == Some(libc::EINPROGRESS) => None,
            ret => {
                let v = ret.map(|_| {
                    let s = self.stream.take().unwrap().into_tcp_stream();
                    TcpStream::from_stream(s, self.io_data.clone())
                });
                Some(v)
            }
        }
    }

    #[inline]
    pub fn done(mut self) -> io::Result<TcpStream> {
        assert_eq!(self.stream.is_none(), false);
        loop {
            co_io_result()?;

            // clear the io_flag
            self.io_data.io_flag.store(false, Ordering::Relaxed);

            match self.stream.as_ref().unwrap().connect(&self.addr.into()) {
                Err(ref e) if e.raw_os_error() == Some(libc::EINPROGRESS) => {}
                Err(ref e) if e.raw_os_error() == Some(libc::EALREADY) => {}
                Err(ref e) if e.raw_os_error() == Some(libc::EISCONN) => {
                    let s = self.stream.take().unwrap().into_tcp_stream();
                    return Ok(TcpStream::from_stream(s, self.io_data.clone()));
                }
                ret => {
                    return ret.map(|_| {
                        let s = self.stream.take().unwrap().into_tcp_stream();
                        TcpStream::from_stream(s, self.io_data.clone())
                    })
                }
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
