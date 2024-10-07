use std::io;
use std::net::SocketAddr;
use std::os::windows::io::AsRawSocket;
#[cfg(feature = "io_timeout")]
use std::time::Duration;

use super::super::miow::{recv_from_overlapped, SocketAddrBuf};
use super::super::{co_io_result, EventData};
#[cfg(feature = "io_cancel")]
use crate::coroutine_impl::co_cancel_data;
use crate::coroutine_impl::{is_coroutine, CoroutineImpl, EventSource};
#[cfg(feature = "io_cancel")]
use crate::io::cancel::CancelIoData;
use crate::net::UdpSocket;
use crate::scheduler::get_scheduler;
use crate::sync::delay_drop::DelayDrop;
use windows_sys::Win32::Foundation::*;

pub struct UdpRecvFrom<'a> {
    io_data: EventData,
    buf: &'a mut [u8],
    socket: &'a ::std::net::UdpSocket,
    addr: SocketAddrBuf,
    #[cfg(feature = "io_timeout")]
    timeout: Option<Duration>,
    can_drop: DelayDrop,
    pub(crate) is_coroutine: bool,
}

impl<'a> UdpRecvFrom<'a> {
    pub fn new(socket: &'a UdpSocket, buf: &'a mut [u8]) -> Self {
        UdpRecvFrom {
            io_data: EventData::new(socket.as_raw_socket() as HANDLE),
            buf,
            socket: socket.inner(),
            addr: SocketAddrBuf::new(),
            #[cfg(feature = "io_timeout")]
            timeout: socket.read_timeout().unwrap(),
            can_drop: DelayDrop::new(),
            is_coroutine: is_coroutine(),
        }
    }

    pub fn done(&mut self) -> io::Result<(usize, SocketAddr)> {
        let size = co_io_result(&self.io_data, self.is_coroutine)?;
        let addr = self.addr.to_socket_addr().ok_or_else(|| {
            io::Error::new(io::ErrorKind::Other, "could not obtain remote address")
        })?;
        Ok((size, addr))
    }
}

impl EventSource for UdpRecvFrom<'_> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let _g = self.can_drop.delay_drop();
        let s = get_scheduler();
        #[cfg(feature = "io_cancel")]
        let cancel = co_cancel_data(&co);
        #[cfg(feature = "io_timeout")]
        if let Some(dur) = self.timeout {
            s.get_selector().add_io_timer(&mut self.io_data, dur);
        }
        // prepare the co first
        self.io_data.co = Some(co);
        // call the overlapped read API
        co_try!(s, self.io_data.co.take().expect("can't get co"), unsafe {
            recv_from_overlapped(
                self.socket.as_raw_socket(),
                self.buf,
                &mut self.addr,
                self.io_data.get_overlapped(),
            )
        });

        #[cfg(feature = "io_cancel")]
        {
            // register the cancel io data
            cancel.set_io(CancelIoData::new(&self.io_data));
            // re-check the cancel status
            if cancel.is_canceled() {
                unsafe { cancel.cancel() };
            }
        }
    }
}
