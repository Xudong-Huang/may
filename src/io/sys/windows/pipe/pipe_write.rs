use std::io;
use std::os::windows::io::{AsRawHandle, RawHandle};
#[cfg(feature = "io_timeout")]
use std::time::Duration;

use super::super::miow::pipe_write_overlapped;
use super::super::{co_io_result, EventData};
use crate::coroutine_impl::{is_coroutine, CoroutineImpl, EventSource};
use crate::scheduler::get_scheduler;

pub struct PipeWrite<'a> {
    io_data: EventData,
    buf: &'a [u8],
    pipe: RawHandle,
    #[cfg(feature = "io_timeout")]
    timeout: Option<Duration>,
    pub(crate) is_coroutine: bool,
}

impl<'a> PipeWrite<'a> {
    pub fn new<T: AsRawHandle>(
        s: &T,
        buf: &'a [u8],
        #[cfg(feature = "io_timeout")] timeout: Option<Duration>,
    ) -> Self {
        let pipe = s.as_raw_handle();
        PipeWrite {
            io_data: EventData::new(pipe),
            buf,
            pipe,
            #[cfg(feature = "io_timeout")]
            timeout,
            is_coroutine: is_coroutine(),
        }
    }

    pub fn done(&mut self) -> io::Result<usize> {
        co_io_result(&self.io_data, self.is_coroutine)
    }
}

impl<'a> EventSource for PipeWrite<'a> {
    #[allow(clippy::needless_return)]
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        #[cfg(feature = "io_timeout")]
        if let Some(dur) = self.timeout {
            s.get_selector().add_io_timer(&mut self.io_data, dur);
        }
        // prepare the co first
        self.io_data.co = Some(co);
        // call the overlapped write API
        co_try!(s, self.io_data.co.take().expect("can't get co"), unsafe {
            pipe_write_overlapped(self.pipe, self.buf, self.io_data.get_overlapped())
        });
    }
}
