use std::io;
use std::net::{SocketAddr, ToSocketAddrs};
use std::os::windows::io::AsRawSocket;
use std::sync::Arc;
#[cfg(feature = "io_timeout")]
use std::time::Duration;

use super::super::miow::send_to_overlapped;
use super::super::{co_io_result, EventData};
#[cfg(feature = "io_cancel")]
use crate::coroutine_impl::co_cancel_data;
use crate::coroutine_impl::{is_coroutine, CoroutineImpl, EventSource};
#[cfg(feature = "io_cancel")]
use crate::io::cancel::CancelIoData;
use crate::net::UdpSocket;
use crate::scheduler::get_scheduler;
use windows_sys::Win32::Foundation::*;

pub struct UdpSendTo<'a> {
    io_data: Arc<EventData>,
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
            .ok_or_else(|| io::Error::other("no socket addresses resolved"))
            .map(|addr| UdpSendTo {
                io_data: Arc::new(EventData::new(socket.as_raw_socket() as HANDLE)),
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
        #[cfg(feature = "io_cancel")]
        let cancel = co_cancel_data(&co);
        #[cfg(feature = "io_timeout")]
        if let Some(dur) = self.timeout {
            s.get_selector().add_io_timer(&self.io_data, dur);
        }
        // this adapter has no `DelayDrop` guard, so once the request is
        // submitted the coroutine can resume on another thread and drop this
        // frame before the checks below; keep the io data reachable locally
        #[cfg(any(feature = "io_timeout", feature = "io_cancel"))]
        let io_data = self.io_data.clone();

        // prepare the co first
        self.io_data.co.store(co);
        // call the overlapped read API
        co_try!(s, self.io_data.co.take().expect("can't get co"), unsafe {
            send_to_overlapped(
                self.socket.as_raw_socket(),
                self.buf,
                &self.addr,
                self.io_data.get_overlapped(),
            )
        });

        // a timer that fired while the request was being submitted could not
        // cancel it yet, finish the cancellation now that it is pending
        #[cfg(feature = "io_timeout")]
        io_data.cancel_if_timed_out();

        #[cfg(feature = "io_cancel")]
        {
            // register the cancel io data; the frame may already be gone, so
            // use the local Arc instead of `self`
            cancel.set_io(CancelIoData::new(&io_data));
            // re-check the cancel status
            if cancel.is_canceled() {
                cancel.cancel();
            }
        }
    }
}
