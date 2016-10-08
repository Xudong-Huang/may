use std;
use std::io;
use std::net::SocketAddr;
use std::time::Duration;
use std::os::windows::io::AsRawSocket;
use super::co_io_result;
use super::super::EventData;
use super::super::winapi::*;
use super::super::miow::net::{UdpSocketExt, SocketAddrBuf};
use net::UdpSocket;
use scheduler::get_scheduler;
use coroutine::{CoroutineImpl, EventSource};

pub struct UdpRecvFrom<'a> {
    io_data: EventData,
    buf: &'a mut [u8],
    socket: &'a std::net::UdpSocket,
    addr: SocketAddrBuf,
    timeout: Option<Duration>,
}

impl<'a> UdpRecvFrom<'a> {
    pub fn new(socket: &'a UdpSocket, buf: &'a mut [u8]) -> Self {
        UdpRecvFrom {
            io_data: EventData::new(socket.as_raw_socket() as HANDLE),
            buf: buf,
            socket: socket.inner(),
            addr: SocketAddrBuf::new(),
            timeout: socket.read_timeout().unwrap(),
        }
    }

    #[inline]
    pub fn done(self) -> io::Result<(usize, SocketAddr)> {
        let size = try!(co_io_result(&self.io_data));
        let addr = try!(self.addr
            .to_socket_addr()
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::Other, "could not obtain remote address")
            }));
        Ok((size, addr))
    }
}

impl<'a> EventSource for UdpRecvFrom<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        // prepare the co first
        self.io_data.co = Some(co);
        // call the overlapped read API
        let ret = co_try!(s, self.io_data.co.take().unwrap(), unsafe {
            self.socket
                .recv_from_overlapped(self.buf, &mut self.addr, self.io_data.get_overlapped())
        });

        // the operation is success, we no need to wait any more
        if ret == true {
            // the done function would contain the actual io size
            // just let the iocp schedule the coroutine
            // s.schedule_io(co);
            return;
        }

        // register the io operaton
        co_try!(s,
                self.io_data.co.take().unwrap(),
                s.add_io(&mut self.io_data, self.timeout));
    }
}
