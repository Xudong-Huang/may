use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle};

use super::super::{co_io_result, EventData};
use coroutine_impl::{CoroutineImpl, EventSource};
use miow::pipe::NamedPipe;
use scheduler::get_scheduler;

pub struct FileWrite<'a> {
    io_data: EventData,
    buf: &'a [u8],
    file: NamedPipe,
}

impl<'a> FileWrite<'a> {
    pub fn new<T: AsRawHandle>(s: &T, buf: &'a [u8]) -> Self {
        let handle = s.as_raw_handle();
        FileWrite {
            io_data: EventData::new(handle),
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
        self.io_data.co = Some(co);
        // call the overlapped write API
        co_try!(s, self.io_data.co.take().expect("can't get co"), unsafe {
            self.file
                .write_overlapped(self.buf, self.io_data.get_overlapped())
        });
    }
}
