use std::io;
use std::os::windows::io::{AsRawSocket, RawSocket};
use std::sync::Arc;
#[cfg(feature = "io_timeout")]
use std::time::Duration;

use super::super::miow::socket_read;
use super::super::{co_io_result, EventData};
#[cfg(feature = "io_cancel")]
use crate::coroutine_impl::co_cancel_data;
use crate::coroutine_impl::{is_coroutine, CoroutineImpl, EventSource};
#[cfg(feature = "io_cancel")]
use crate::io::cancel::CancelIoData;
use crate::scheduler::get_scheduler;
use crate::sync::delay_drop::DelayDrop;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Networking::WinSock::MSG_PEEK;

pub struct SocketPeek<'a> {
    io_data: Arc<EventData>,
    buf: &'a mut [u8],
    socket: RawSocket,
    #[cfg(feature = "io_timeout")]
    timeout: Option<Duration>,
    can_drop: DelayDrop,
    pub(crate) is_coroutine: bool,
}

impl<'a> SocketPeek<'a> {
    pub fn new<T: AsRawSocket>(
        s: &T,
        buf: &'a mut [u8],
        #[cfg(feature = "io_timeout")] timeout: Option<Duration>,
    ) -> Self {
        let socket = s.as_raw_socket();
        SocketPeek {
            io_data: Arc::new(EventData::new(socket as HANDLE)),
            buf,
            socket,
            #[cfg(feature = "io_timeout")]
            timeout,
            can_drop: DelayDrop::new(),
            is_coroutine: is_coroutine(),
        }
    }

    pub fn done(&mut self) -> io::Result<usize> {
        co_io_result(&self.io_data, self.is_coroutine)
    }
}

impl EventSource for SocketPeek<'_> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        #[cfg(feature = "io_cancel")]
        let cancel = co_cancel_data(&co);
        let _g = self.can_drop.delay_drop();
        // prepare the timer before the API: if it fires while the request is
        // still being submitted it is completed by `cancel_if_timed_out`
        #[cfg(feature = "io_timeout")]
        if let Some(dur) = self.timeout {
            s.get_selector().add_io_timer(&self.io_data, dur);
        }

        // prepare the co first
        self.io_data.co.store(co);

        // call the overlapped read API
        co_try!(s, self.io_data.co.take().expect("can't get co"), unsafe {
            socket_read(
                self.socket,
                self.buf,
                MSG_PEEK,
                self.io_data.get_overlapped(),
            )
        });

        // a timer that fired while the request was being submitted could not
        // cancel it yet, finish the cancellation now that it is pending; the
        // `can_drop` guard keeps this frame alive until `subscribe` returns
        #[cfg(feature = "io_timeout")]
        self.io_data.cancel_if_timed_out();

        #[cfg(feature = "io_cancel")]
        {
            // register the cancel io data
            cancel.set_io(CancelIoData::new(&self.io_data));
            // re-check the cancel status
            if cancel.is_canceled() {
                cancel.cancel();
            }
        }
    }
}
