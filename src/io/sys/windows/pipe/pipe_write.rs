use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle};
use std::time::Duration;

use super::super::{co_io_result, EventData};
use coroutine_impl::{CoroutineImpl, EventSource};
use miow::pipe::NamedPipe;
use scheduler::get_scheduler;

pub struct PipeWrite<'a> {
    io_data: EventData,
    buf: &'a [u8],
    pipe: NamedPipe,
    timeout: Option<Duration>,
}

impl<'a> PipeWrite<'a> {
    pub fn new<T: AsRawHandle>(s: &T, buf: &'a [u8], timeout: Option<Duration>) -> Self {
        let handle = s.as_raw_handle();
        PipeWrite {
            io_data: EventData::new(handle),
            buf,
            pipe: unsafe { FromRawHandle::from_raw_handle(handle) },
            timeout,
        }
    }

    #[inline]
    pub fn done(self) -> io::Result<usize> {
        // don't close the socket
        self.pipe.into_raw_handle();
        co_io_result(&self.io_data)
    }
}

impl<'a> EventSource for PipeWrite<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        s.get_selector()
            .add_io_timer(&mut self.io_data, self.timeout);
        // prepare the co first
        self.io_data.co = Some(co);
        // call the overlapped write API
        co_try!(s, self.io_data.co.take().expect("can't get co"), unsafe {
            self.pipe
                .write_overlapped(self.buf, self.io_data.get_overlapped())
        });
    }
}
