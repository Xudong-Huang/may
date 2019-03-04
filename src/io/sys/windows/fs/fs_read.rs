use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle};

use super::super::{co_io_result, EventData};
use coroutine_impl::{co_cancel_data, CoroutineImpl, EventSource};
use io::cancel::CancelIoData;
use miow::pipe::NamedPipe;
use scheduler::get_scheduler;
use sync::delay_drop::DelayDrop;

pub struct FileRead<'a> {
    io_data: EventData,
    buf: &'a mut [u8],
    file: NamedPipe,
    can_drop: DelayDrop,
}

impl<'a> FileRead<'a> {
    pub fn new<T: AsRawHandle>(f: &T, buf: &'a mut [u8]) -> Self {
        let handle = f.as_raw_handle();
        FileRead {
            io_data: EventData::new(handle),
            buf,
            file: unsafe { FromRawHandle::from_raw_handle(handle) },
            can_drop: DelayDrop::new(),
        }
    }

    #[inline]
    pub fn done(self) -> io::Result<usize> {
        // don't close the file
        self.file.into_raw_handle();
        co_io_result(&self.io_data)
    }
}

impl<'a> EventSource for FileRead<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        let cancel = co_cancel_data(&co);
        let _g = self.can_drop.delay_drop();

        // prepare the co first
        self.io_data.co = Some(co);

        // call the overlapped read API
        co_try!(s, self.io_data.co.take().expect("can't get co"), unsafe {
            self.file
                .read_overlapped(self.buf, self.io_data.get_overlapped())
        });

        // register the cancel io data
        cancel.set_io(CancelIoData::new(&self.io_data));
        // re-check the cancel status
        if cancel.is_canceled() {
            unsafe { cancel.cancel() };
        }
    }
}
