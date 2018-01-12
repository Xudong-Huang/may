use std::io;
use std::time::Duration;
use std::os::windows::io::AsRawSocket;
use std::net::TcpStream as SysTcpStream;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6, ToSocketAddrs};
use net::TcpStream;
use miow::net::TcpStreamExt;
use winapi::shared::ntdef::*;
use scheduler::get_scheduler;
use io::cancel::CancelIoData;
use sync::delay_drop::DelayDrop;
use super::super::{add_socket, co_io_result, EventData, IoData};
use coroutine_impl::{co_cancel_data, CoroutineImpl, EventSource};

pub struct TcpStreamConnect {
    io_data: EventData,
    stream: SysTcpStream,
    addr: SocketAddr,
    can_drop: DelayDrop,
}

impl TcpStreamConnect {
    pub fn new<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        use socket2::{Domain, Socket, Type};

        let err = io::Error::new(io::ErrorKind::Other, "no socket addresses resolved");
        addr.to_socket_addrs()?
            .fold(Err(err), |prev, addr| {
                prev.or_else(|_| {
                    let socket = match addr {
                        SocketAddr::V4(..) => Socket::new(Domain::ipv4(), Type::stream(), None)?,
                        SocketAddr::V6(..) => Socket::new(Domain::ipv4(), Type::stream(), None)?,
                    };
                    Ok((socket, addr))
                })
            })
            .and_then(|(socket, addr)| {
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

                socket
                    .bind(&any.into())
                    .and_then(|_| Ok(socket.into_tcp_stream()))
                    .and_then(|s| {
                        // must register io first
                        s.set_nonblocking(true)?;
                        add_socket(&s).map(|_io| TcpStreamConnect {
                            io_data: EventData::new(s.as_raw_socket() as HANDLE),
                            addr: addr,
                            stream: s,
                            can_drop: DelayDrop::new(),
                        })
                    })
            })
    }

    #[inline]
    pub fn is_connected(&mut self) -> io::Result<bool> {
        // always try overvlappend version
        Ok(false)
    }

    #[inline]
    pub fn done(self) -> io::Result<TcpStream> {
        co_io_result(&self.io_data)?;
        self.stream.connect_complete()?;
        Ok(TcpStream::from_stream(self.stream, IoData))
    }
}

impl EventSource for TcpStreamConnect {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let _g = self.can_drop.delay_drop();
        let s = get_scheduler();
        let cancel = co_cancel_data(&co);
        s.get_selector()
            .add_io_timer(&mut self.io_data, Some(Duration::from_secs(10)));
        self.io_data.co = Some(co);

        // call the overlapped connect API
        co_try!(s, self.io_data.co.take().expect("can't get co"), unsafe {
            self.stream
                .connect_overlapped(&self.addr, &[], self.io_data.get_overlapped())
        });

        // register the cancel io data
        cancel.set_io(CancelIoData::new(&self.io_data));
        // re-check the cancel status
        if cancel.is_canceled() {
            unsafe { cancel.cancel() };
        }
    }
}
