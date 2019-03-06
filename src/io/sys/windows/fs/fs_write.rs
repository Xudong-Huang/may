use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle};
use std::sync::atomic::Ordering;

use super::super::{co_io_result, EventData};
use coroutine_impl::{CoroutineImpl, EventSource};
use miow::pipe::NamedPipe;
use miow::Overlapped;
use scheduler::get_scheduler;

pub struct FileWrite<'a> {
    io_data: EventData,
    buf: &'a [u8],
    file: NamedPipe,
}

impl<'a> FileWrite<'a> {
    pub fn new<T: AsRawHandle>(s: &T, offset: u64, buf: &'a [u8]) -> Self {
        let handle = s.as_raw_handle();
        let mut io_data = EventData::new(handle);
        let overlapped = unsafe { Overlapped::from_raw(io_data.get_overlapped()) };
        overlapped.set_offset(offset);
        FileWrite {
            io_data,
            buf,
            file: unsafe { FromRawHandle::from_raw_handle(handle) },
        }
    }

    #[inline]
    pub fn done(self) -> io::Result<usize> {
        // don't close the file
        self.file.into_raw_handle();
        co_io_result(&self.io_data)
    }
}

impl<'a> EventSource for FileWrite<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        // prepare the co first
        self.io_data.co.swap(co, Ordering::Release);
        // call the overlapped write API
        co_try!(s, self.io_data.co.take(Ordering::AcqRel), unsafe {
            self.file
                .write_overlapped(self.buf, self.io_data.get_overlapped())
        });
    }
}
