use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, RawHandle};
use std::time::Duration;

use super::super::{co_io_result, EventData};
use crate::coroutine_impl::{CoroutineImpl, EventSource};
use crate::scheduler::get_scheduler;
use miow::pipe::NamedPipe;

pub struct PipeWrite<'a> {
    io_data: EventData,
    buf: &'a [u8],
    pipe: RawHandle,
    timeout: Option<Duration>,
}

impl<'a> PipeWrite<'a> {
    pub fn new<T: AsRawHandle>(s: &T, buf: &'a [u8], timeout: Option<Duration>) -> Self {
        let pipe = s.as_raw_handle();
        PipeWrite {
            io_data: EventData::new(pipe),
            buf,
            pipe,
            timeout,
        }
    }

    pub fn done(&mut self) -> io::Result<usize> {
        co_io_result(&self.io_data)
    }
}

impl<'a> EventSource for PipeWrite<'a> {
    #[allow(clippy::needless_return)]
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        if let Some(dur) = self.timeout {
            s.get_selector().add_io_timer(&mut self.io_data, dur);
        }
        // prepare the co first
        self.io_data.co = Some(co);
        // call the overlapped write API
        co_try!(s, self.io_data.co.take().expect("can't get co"), unsafe {
            let pipe: NamedPipe = FromRawHandle::from_raw_handle(self.pipe);
            let ret = pipe.write_overlapped(self.buf, self.io_data.get_overlapped());
            // don't close the socket
            pipe.into_raw_handle();
            ret
        });
    }
}
