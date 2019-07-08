use std::io;
use std::net::SocketAddr;
use std::os::windows::io::AsRawSocket;
use std::time::Duration;

use super::super::{co_io_result, EventData};
use crate::coroutine_impl::{co_cancel_data, CoroutineImpl, EventSource};
use crate::io::cancel::CancelIoData;
use crate::net::UdpSocket;
use crate::scheduler::get_scheduler;
use crate::sync::delay_drop::DelayDrop;
use miow::net::{SocketAddrBuf, UdpSocketExt};
use winapi::shared::ntdef::*;

pub struct UdpRecvFrom<'a> {
    io_data: EventData,
    buf: &'a mut [u8],
    socket: &'a ::std::net::UdpSocket,
    addr: SocketAddrBuf,
    timeout: Option<Duration>,
    can_drop: DelayDrop,
}

impl<'a> UdpRecvFrom<'a> {
    pub fn new(socket: &'a UdpSocket, buf: &'a mut [u8]) -> Self {
        UdpRecvFrom {
            io_data: EventData::new(socket.as_raw_socket() as HANDLE),
            buf,
            socket: socket.inner(),
            addr: SocketAddrBuf::new(),
            timeout: socket.read_timeout().unwrap(),
            can_drop: DelayDrop::new(),
        }
    }

    pub fn done(&mut self) -> io::Result<(usize, SocketAddr)> {
        let size = co_io_result(&self.io_data)?;
        let addr = self.addr.to_socket_addr().ok_or_else(|| {
            io::Error::new(io::ErrorKind::Other, "could not obtain remote address")
        })?;
        Ok((size, addr))
    }
}

impl<'a> EventSource for UdpRecvFrom<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let _g = self.can_drop.delay_drop();
        let s = get_scheduler();
        let cancel = co_cancel_data(&co);
        if let Some(dur) = self.timeout {
            s.get_selector().add_io_timer(&mut self.io_data, dur);
        }
        // prepare the co first
        self.io_data.co = Some(co);
        // call the overlapped read API
        co_try!(s, self.io_data.co.take().expect("can't get co"), unsafe {
            self.socket.recv_from_overlapped(
                self.buf,
                &mut self.addr,
                self.io_data.get_overlapped(),
            )
        });

        // register the cancel io data
        cancel.set_io(CancelIoData::new(&self.io_data));
        // re-check the cancel status
        if cancel.is_canceled() {
            #[cold]
            unsafe {
                cancel.cancel()
            };
        }
    }
}
