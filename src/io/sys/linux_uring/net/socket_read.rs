use std::io;
use std::sync::Arc;
#[cfg(feature = "io_timeout")]
use std::time::Duration;

use io_uring::{opcode, types};

use super::super::event::{co_io_result, EventData};
#[cfg(feature = "io_cancel")]
use crate::coroutine_impl::co_cancel_data;
use crate::coroutine_impl::{is_coroutine, CoroutineImpl, EventSource};
#[cfg(feature = "io_cancel")]
use crate::io::sys::cancel::CancelIoData;
use crate::io::AsIoData;
use crate::scheduler::get_scheduler;
use crate::sync::delay_drop::DelayDrop;

pub struct SocketRead<'a> {
    event_data: Arc<EventData>,
    buf: &'a mut [u8],
    #[cfg(feature = "io_timeout")]
    timeout: Option<Duration>,
    can_drop: DelayDrop,
    pub(crate) is_coroutine: bool,
}

impl<'a> SocketRead<'a> {
    pub fn new<T: AsIoData>(
        s: &'a T,
        buf: &'a mut [u8],
        #[cfg(feature = "io_timeout")] timeout: Option<Duration>,
    ) -> Self {
        SocketRead {
            event_data: s.as_io_data().new_request(),
            buf,
            #[cfg(feature = "io_timeout")]
            timeout,
            can_drop: DelayDrop::new(),
            is_coroutine: is_coroutine(),
        }
    }

    pub fn done(&mut self) -> io::Result<usize> {
        co_io_result(&self.event_data, self.is_coroutine)
    }
}

impl EventSource for SocketRead<'_> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        #[cfg(feature = "io_cancel")]
        let cancel = co_cancel_data(&co);
        let _g = self.can_drop.delay_drop();

        // The timer is armed before submission so a zero-duration timeout
        // cannot be lost; `Selector::submit` completes the cancellation after
        // the SQE is live if the timer fired first.
        #[cfg(feature = "io_timeout")]
        if let Some(dur) = self.timeout {
            s.get_selector().add_io_timer(&self.event_data, dur);
        }

        // `offset(-1)` means "use the current file position", which is the
        // `read(2)` behaviour CoIo relies on; sockets and pipes accept it too.
        let entry = opcode::Read::new(
            types::Fd(self.event_data.fd()),
            self.buf.as_mut_ptr(),
            self.buf.len() as u32,
        )
        .offset(u64::MAX)
        .build();

        #[cfg(feature = "io_cancel")]
        let event_data = submit_co!(s, &self.event_data, co, entry);
        #[cfg(not(feature = "io_cancel"))]
        submit_co!(s, &self.event_data, co, entry);

        #[cfg(feature = "io_cancel")]
        {
            cancel.set_io(CancelIoData::new(&event_data));
            if cancel.is_canceled() {
                cancel.cancel();
            }
        }
    }
}
