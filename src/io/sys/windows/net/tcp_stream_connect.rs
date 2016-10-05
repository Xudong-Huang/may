use std::io;
use std::time::Duration;
use std::os::windows::io::AsRawSocket;
use std::net::{ToSocketAddrs, SocketAddr, SocketAddrV4, SocketAddrV6, Ipv4Addr, Ipv6Addr};
use super::co_io_result;
use super::super::EventData;
use super::super::winapi::*;
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
        let builder_addr = try!(addr.to_socket_addrs()).fold(Err(err), |prev, addr| {
            prev.or_else(|_| {
                let builder = match addr {
                    SocketAddr::V4(..) => try!(TcpBuilder::new_v4()),
                    SocketAddr::V6(..) => try!(TcpBuilder::new_v6()),
                };
                Ok((builder, addr))
            })
        });

        let (builder, addr) = try!(builder_addr);
        // windows need to bind first when call ConnectEx API
        let any: SocketAddr = match addr {
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

        try!(builder.bind(&any));
        let s = try!(builder.to_tcp_stream());
        // must use the new to register io
        let s = try!(TcpStream::new(s));
        Ok(TcpStreamConnect {
            io_data: EventData::new(s.as_raw_socket() as HANDLE),
            addr: addr,
            stream: s,
        })
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
        // prepare the co first
        self.io_data.co = Some(co);

        // call the overlapped connect API
        let ret = co_try!(s, self.io_data.co.take().unwrap(), unsafe {
            self.stream.inner().connect_overlapped(&self.addr, self.io_data.get_overlapped())
        });

        // the operation is success, we no need to wait any more
        if ret == true {
            return;
        }

        // register the io operaton, by default the timeout is 10 sec
        co_try!(s,
                self.io_data.co.take().unwrap(),
                s.add_io(&mut self.io_data, Some(Duration::from_secs(10))));
    }
}
