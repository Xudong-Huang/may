use std;
use std::io;
use std::net::{SocketAddr, ToSocketAddrs};
use std::time::Duration;
use std::os::windows::io::AsRawSocket;
use super::super::winapi::*;
use super::super::miow::net::UdpSocketExt;
use super::super::{co_io_result, EventData};
use net::UdpSocket;
use scheduler::get_scheduler;
use coroutine_impl::{CoroutineImpl, EventSource};

pub struct UdpSendTo<'a> {
    io_data: EventData,
    buf: &'a [u8],
    socket: &'a std::net::UdpSocket,
    addr: SocketAddr,
    timeout: Option<Duration>,
}

impl<'a> UdpSendTo<'a> {
    pub fn new<A: ToSocketAddrs>(
        socket: &'a UdpSocket,
        buf: &'a [u8],
        addr: A,
    ) -> io::Result<Self> {
        let err = io::Error::new(io::ErrorKind::Other, "no socket addresses resolved");
        try!(addr.to_socket_addrs())
            .fold(Err(err), |prev, addr| prev.or_else(|_| Ok(addr)))
            .map(|addr| UdpSendTo {
                io_data: EventData::new(socket.as_raw_socket() as HANDLE),
                buf: buf,
                socket: socket.inner(),
                addr: addr,
                timeout: socket.write_timeout().unwrap(),
            })
    }

    #[inline]
    pub fn done(self) -> io::Result<usize> {
        co_io_result(&self.io_data)
    }
}

impl<'a> EventSource for UdpSendTo<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        s.get_selector()
            .add_io_timer(&mut self.io_data, self.timeout);
        // prepare the co first
        self.io_data.co = Some(co);
        // call the overlapped read API
        co_try!(s, self.io_data.co.take().expect("can't get co"), unsafe {
            self.socket
                .send_to_overlapped(self.buf, &self.addr, self.io_data.get_overlapped())
        });
    }
}
