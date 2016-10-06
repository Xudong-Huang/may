use std;
use std::io;
use std::net::SocketAddr;
use std::os::windows::io::AsRawSocket;
use super::co_io_result;
use super::super::EventData;
use super::super::winapi::*;
use super::super::miow::net::{TcpListenerExt, AcceptAddrsBuf};
use net2::TcpBuilder;
use net::{TcpStream, TcpListener};
use scheduler::get_scheduler;
use coroutine::{CoroutineImpl, EventSource};

pub struct TcpListenerAccept<'a> {
    io_data: EventData,
    socket: &'a std::net::TcpListener,
    builder: TcpBuilder,
    ret: Option<std::net::TcpStream>,
    addr: AcceptAddrsBuf,
}

impl<'a> TcpListenerAccept<'a> {
    pub fn new(socket: &'a TcpListener) -> io::Result<Self> {
        let addr = try!(socket.local_addr());
        let builder = match addr {
            SocketAddr::V4(..) => try!(TcpBuilder::new_v4()),
            SocketAddr::V6(..) => try!(TcpBuilder::new_v6()),
        };

        Ok(TcpListenerAccept {
            io_data: EventData::new(socket.as_raw_socket() as HANDLE),
            socket: socket.inner(),
            builder: builder,
            ret: None,
            addr: AcceptAddrsBuf::new(),
        })
    }

    #[inline]
    pub fn done(self) -> io::Result<(TcpStream, SocketAddr)> {
        try!(co_io_result(&self.io_data));

        let s = try!(self.ret
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "tcp listener ret is not set"))
            .and_then(|s| TcpStream::new(s)));

        let addr = try!(self.addr.parse(&self.socket).and_then(|a| {
            a.remote().ok_or_else(|| {
                io::Error::new(io::ErrorKind::Other, "could not obtain remote address")
            })
        }));

        Ok((s, addr))
    }
}

impl<'a> EventSource for TcpListenerAccept<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        // prepare the co first
        self.io_data.co = Some(co);

        // call the overlapped read API
        let (s, _) = co_try!(s, self.io_data.co.take().unwrap(), unsafe {
            self.socket
                .accept_overlapped(&self.builder, &mut self.addr, self.io_data.get_overlapped())
        });

        self.ret = Some(s);

        // we don't need to register the timeout here,
        // windows add_io is mainly used for that
        // the API already regiser the io
    }
}
