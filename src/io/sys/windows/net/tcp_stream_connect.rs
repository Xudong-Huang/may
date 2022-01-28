use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6, ToSocketAddrs};
use std::os::windows::io::AsRawSocket;
use std::time::Duration;

use super::super::{add_socket, co_io_result, EventData, IoData};
use crate::coroutine_impl::{co_cancel_data, CoroutineImpl, EventSource};
use crate::io::cancel::CancelIoData;
use crate::io::OptionCell;
use crate::net::TcpStream;
use crate::scheduler::get_scheduler;
use crate::sync::delay_drop::DelayDrop;
use miow::net::TcpStreamExt;
use windows_sys::Win32::Foundation::*;

pub struct TcpStreamConnect {
    io_data: EventData,
    stream: OptionCell<std::net::TcpStream>,
    timeout: Option<Duration>,
    addr: SocketAddr,
    can_drop: DelayDrop,
}

impl TcpStreamConnect {
    pub fn new<A: ToSocketAddrs>(addr: A, timeout: Option<Duration>) -> io::Result<Self> {
        use socket2::{Domain, Socket, Type};

        let err = io::Error::new(io::ErrorKind::Other, "no socket addresses resolved");
        // TODO:
        // resolve addr is a blocking operation!
        // here we should use a thread to finish the resolve?
        addr.to_socket_addrs()?
            .fold(Err(err), |prev, addr| {
                prev.or_else(|_| {
                    let socket = match addr {
                        SocketAddr::V4(..) => Socket::new(Domain::IPV4, Type::STREAM, None)?,
                        SocketAddr::V6(..) => Socket::new(Domain::IPV6, Type::STREAM, None)?,
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

                socket.bind(&any.into()).map(|_| socket.into()).and_then(
                    |s: std::net::TcpStream| {
                        // must register io first
                        s.set_nonblocking(true)?;
                        add_socket(&s).map(|_io| TcpStreamConnect {
                            io_data: EventData::new(s.as_raw_socket() as HANDLE),
                            addr,
                            stream: OptionCell::new(s),
                            timeout,
                            can_drop: DelayDrop::new(),
                        })
                    },
                )
            })
    }

    pub fn done(&mut self) -> io::Result<TcpStream> {
        co_io_result(&self.io_data)?;
        let stream = self.stream.take();
        stream.connect_complete()?;
        Ok(TcpStream::from_stream(stream, IoData))
    }
}

impl EventSource for TcpStreamConnect {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let _g = self.can_drop.delay_drop();
        let s = get_scheduler();
        let cancel = co_cancel_data(&co);
        if let Some(dur) = self.timeout {
            s.get_selector().add_io_timer(&mut self.io_data, dur);
        }
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
