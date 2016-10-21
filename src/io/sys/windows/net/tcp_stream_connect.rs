use std::io;
use std::time::Duration;
use std::os::windows::io::AsRawSocket;
use std::net::{ToSocketAddrs, SocketAddr, SocketAddrV4, SocketAddrV6, Ipv4Addr, Ipv6Addr};
use super::super::winapi::*;
use super::super::EventData;
use super::super::co_io_result;
use super::super::miow::net::TcpStreamExt;
use net::TcpStream;
use net2::TcpBuilder;
use scheduler::get_scheduler;
use coroutine::{CoroutineImpl, EventSource};

pub struct TcpStreamConnect {
    io_data: EventData,
    stream: TcpStream,
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
                // windows need to bind first when call ConnectEx API
                let any = match addr {
                    SocketAddr::V4(..) => {
                        let any = Ipv4Addr::new(0, 0, 0, 0);
                        let addr = SocketAddrV4::new(any, 0);
                        SocketAddr::V4(addr)
                    }
                    SocketAddr::V6(..) => {
                        let any = Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 0);
                        let addr = SocketAddrV6::new(any, 0, 0, 0);
                        SocketAddr::V6(addr)
                    }
                };

                builder.bind(&any).and_then(|_| builder.to_tcp_stream()).and_then(|s| {
                    // must use the new to register io
                    TcpStream::new(s).map(|s| {
                        TcpStreamConnect {
                            io_data: EventData::new(s.as_raw_socket() as HANDLE),
                            addr: addr,
                            stream: s,
                        }
                    })
                })
            })
    }

    #[inline]
    pub fn get_stream(&mut self) -> Option<io::Result<TcpStream>> {
        // always try overvlappend version
        None
    }

    #[inline]
    pub fn done(self) -> io::Result<TcpStream> {
        try!(co_io_result(&self.io_data));
        try!(self.stream.inner().connect_complete());
        Ok(self.stream)
    }
}

impl EventSource for TcpStreamConnect {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        s.get_selector().add_io_timer(&mut self.io_data, Some(Duration::from_secs(10)));
        self.io_data.co = Some(co);

        // call the overlapped connect API
        co_try!(s, self.io_data.co.take().expect("can't get co"), unsafe {
            self.stream.inner().connect_overlapped(&self.addr, self.io_data.get_overlapped())
        });
    }
}
