use std::io;
use std::net::SocketAddr;
use std::os::windows::io::AsRawSocket;

use super::super::{add_socket, co_io_result, EventData};
#[cfg(feature = "io_cancel")]
use crate::coroutine_impl::co_cancel_data;
use crate::coroutine_impl::{is_coroutine, CoroutineImpl, EventSource};
#[cfg(feature = "io_cancel")]
use crate::io::cancel::CancelIoData;
use crate::io::OptionCell;
use crate::net::{TcpListener, TcpStream};
use crate::scheduler::get_scheduler;
use crate::sync::delay_drop::DelayDrop;
use miow::net::{AcceptAddrsBuf, TcpListenerExt};
use windows_sys::Win32::Foundation::*;

pub struct TcpListenerAccept<'a> {
    io_data: EventData,
    socket: &'a ::std::net::TcpListener,
    ret: OptionCell<::std::net::TcpStream>,
    addr: AcceptAddrsBuf,
    can_drop: DelayDrop,
    pub(crate) is_coroutine: bool,
}

impl<'a> TcpListenerAccept<'a> {
    pub fn new(socket: &'a TcpListener) -> io::Result<Self> {
        use socket2::{Domain, Socket, Type};

        let local_addr = socket.local_addr()?;
        let stream = match local_addr {
            SocketAddr::V4(..) => Socket::new(Domain::IPV4, Type::STREAM, None)?,
            SocketAddr::V6(..) => Socket::new(Domain::IPV6, Type::STREAM, None)?,
        };
        let stream = stream.into();

        Ok(TcpListenerAccept {
            io_data: EventData::new(socket.as_raw_socket() as HANDLE),
            socket: socket.inner(),
            ret: OptionCell::new(stream),
            addr: AcceptAddrsBuf::new(),
            can_drop: DelayDrop::new(),
            is_coroutine: is_coroutine(),
        })
    }

    pub fn done(&mut self) -> io::Result<(TcpStream, SocketAddr)> {
        co_io_result(&self.io_data, self.is_coroutine)?;
        let socket = &self.socket;
        let ss = self.ret.take();
        let s = socket.accept_complete(&ss).and_then(|_| {
            ss.set_nonblocking(true)?;
            add_socket(&ss).map(|io| TcpStream::from_stream(ss, io))
        })?;

        let addr = self.addr.parse(self.socket).and_then(|a| {
            a.remote().ok_or_else(|| {
                io::Error::new(io::ErrorKind::Other, "could not obtain remote address")
            })
        })?;

        Ok((s, addr))
    }
}

impl<'a> EventSource for TcpListenerAccept<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let _g = self.can_drop.delay_drop();
        let s = get_scheduler();
        #[cfg(feature = "io_cancel")]
        let cancel = co_cancel_data(&co);
        // we don't need to register the timeout here,
        // prepare the co first
        self.io_data.co = Some(co);

        // call the overlapped read API
        co_try!(s, self.io_data.co.take().expect("can't get co"), unsafe {
            self.socket
                .accept_overlapped(&self.ret, &mut self.addr, self.io_data.get_overlapped())
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
