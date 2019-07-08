use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, RawHandle};
use std::time::Duration;

use super::super::{co_io_result, EventData};
use crate::coroutine_impl::{co_cancel_data, CoroutineImpl, EventSource};
use crate::io::cancel::CancelIoData;
use crate::scheduler::get_scheduler;
use crate::sync::delay_drop::DelayDrop;
use miow::pipe::NamedPipe;
use winapi::shared::winerror::*;

pub struct PipeRead<'a> {
    io_data: EventData,
    buf: &'a mut [u8],
    pipe: RawHandle,
    timeout: Option<Duration>,
    can_drop: DelayDrop,
}

impl<'a> PipeRead<'a> {
    pub fn new<T: AsRawHandle>(s: &T, buf: &'a mut [u8], timeout: Option<Duration>) -> Self {
        let pipe = s.as_raw_handle();
        PipeRead {
            io_data: EventData::new(pipe),
            buf,
            pipe,
            timeout,
            can_drop: DelayDrop::new(),
        }
    }

    pub fn done(&mut self) -> io::Result<usize> {
        match co_io_result(&self.io_data) {
            // we should treat the broken pipe as read to end
            Err(ref e) if Some(ERROR_BROKEN_PIPE as i32) == e.raw_os_error() => Ok(0),
            ret => ret,
        }
    }
}

impl<'a> EventSource for PipeRead<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        let cancel = co_cancel_data(&co);
        let _g = self.can_drop.delay_drop();
        // if the event happened before this there would be something wrong
        // that the timer handle can't be removed in time
        // we must prepare the timer before call the API
        if let Some(dur) = self.timeout {
            s.get_selector().add_io_timer(&mut self.io_data, dur);
        }
        // prepare the co first
        self.io_data.co = Some(co);

        // call the overlapped read API
        co_try!(s, self.io_data.co.take().expect("can't get co"), unsafe {
            let pipe: NamedPipe = FromRawHandle::from_raw_handle(self.pipe);
            let ret = pipe.read_overlapped(self.buf, self.io_data.get_overlapped());
            // don't close the socket
            pipe.into_raw_handle();
            ret
        });

        // register the cancel io data
        cancel.set_io(CancelIoData::new(&self.io_data));
        // re-check the cancel status
        if cancel.is_canceled() {
            #[cold]
            unsafe {
                cancel.cancel()
            };
        }
    }
}
