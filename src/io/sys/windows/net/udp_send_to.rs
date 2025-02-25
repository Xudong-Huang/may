use std::io;
use std::net::{SocketAddr, ToSocketAddrs};
use std::os::windows::io::AsRawSocket;
#[cfg(feature = "io_timeout")]
use std::time::Duration;

use super::super::miow::send_to_overlapped;
use super::super::{co_io_result, EventData};
use crate::coroutine_impl::{is_coroutine, CoroutineImpl, EventSource};
use crate::net::UdpSocket;
use crate::scheduler::get_scheduler;
use windows_sys::Win32::Foundation::*;

pub struct UdpSendTo<'a> {
    io_data: EventData,
    buf: &'a [u8],
    socket: &'a ::std::net::UdpSocket,
    addr: SocketAddr,
    #[cfg(feature = "io_timeout")]
    timeout: Option<Duration>,
    pub(crate) is_coroutine: bool,
}

impl<'a> UdpSendTo<'a> {
    pub fn new<A: ToSocketAddrs>(
        socket: &'a UdpSocket,
        buf: &'a [u8],
        addr: A,
    ) -> io::Result<Self> {
        addr.to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "no socket addresses resolved"))
            .map(|addr| UdpSendTo {
                io_data: EventData::new(socket.as_raw_socket() as HANDLE),
                buf,
                socket: socket.inner(),
                addr,
                #[cfg(feature = "io_timeout")]
                timeout: socket.write_timeout().unwrap(),
                is_coroutine: is_coroutine(),
            })
    }

    pub fn done(&mut self) -> io::Result<usize> {
        co_io_result(&self.io_data, self.is_coroutine)
    }
}

impl EventSource for UdpSendTo<'_> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        #[cfg(feature = "io_timeout")]
        if let Some(dur) = self.timeout {
            s.get_selector().add_io_timer(&mut self.io_data, dur);
        }
        // prepare the co first
        self.io_data.co = Some(co);
        // call the overlapped read API
        co_try!(s, self.io_data.co.take().expect("can't get co"), unsafe {
            send_to_overlapped(
                self.socket.as_raw_socket(),
                self.buf,
                &self.addr,
                self.io_data.get_overlapped(),
            )
        });
    }
}
