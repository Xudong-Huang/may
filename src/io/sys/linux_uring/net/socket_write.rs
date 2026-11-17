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

pub struct SocketWrite<'a> {
    event_data: Arc<EventData>,
    buf: &'a [u8],
    #[cfg(feature = "io_timeout")]
    timeout: Option<Duration>,
    pub(crate) is_coroutine: bool,
}

impl<'a> SocketWrite<'a> {
    pub fn new<T: AsIoData>(
        s: &'a T,
        buf: &'a [u8],
        #[cfg(feature = "io_timeout")] timeout: Option<Duration>,
    ) -> Self {
        SocketWrite {
            event_data: s.as_io_data().new_request(),
            buf,
            #[cfg(feature = "io_timeout")]
            timeout,
            is_coroutine: is_coroutine(),
        }
    }

    pub fn done(&mut self) -> io::Result<usize> {
        co_io_result(&self.event_data, self.is_coroutine)
    }
}

impl EventSource for SocketWrite<'_> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        #[cfg(feature = "io_cancel")]
        let cancel = co_cancel_data(&co);

        #[cfg(feature = "io_timeout")]
        if let Some(dur) = self.timeout {
            s.get_selector().add_io_timer(&self.event_data, dur);
        }

        // `offset(-1)` selects the current file position like `write(2)`.
        let entry = opcode::Write::new(
            types::Fd(self.event_data.fd()),
            self.buf.as_ptr(),
            self.buf.len() as u32,
        )
        .offset(u64::MAX)
        .build();

        // No `DelayDrop`: the macro keeps the request alive locally, and this
        // adapter touches nothing from `self` after submission.
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
