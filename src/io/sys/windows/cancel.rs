use std::io;
use std::sync::atomic::{AtomicPtr, Ordering};

use super::EventData;
use cancel::CancelIo;

pub struct CancelIoData {
    ev_data: *mut EventData,
}

impl CancelIoData {
    pub fn new(ev_data: &EventData) -> Self {
        CancelIoData {
            ev_data: ev_data as *const _ as *mut _,
        }
    }

    pub unsafe fn cancel(&self) -> io::Result<()> {
        use winapi::um::ioapiset::CancelIoEx;

        let ev = &mut *self.ev_data;
        let handle = ev.handle;
        let overlapped = ev.get_overlapped();
        let ret = CancelIoEx(handle, overlapped);
        if ret == 0 {
            let err = io::Error::last_os_error();
            error!("cancel err={:?}", err);
            // ev.co.take().map(|co| get_scheduler().schedule(co));
            Err(err)
        } else {
            Ok(())
        }
    }
}

pub struct CancelIoImpl(AtomicPtr<EventData>);

impl CancelIo for CancelIoImpl {
    type Data = CancelIoData;

    fn new() -> Self {
        CancelIoImpl(AtomicPtr::default())
    }

    fn set(&self, data: CancelIoData) {
        self.0.store(data.ev_data, Ordering::Release);
    }

    fn clear(&self) {
        self.0.store(::std::ptr::null_mut(), Ordering::Release);
    }

    unsafe fn cancel(&self) {
        let ptr = self.0.swap(::std::ptr::null_mut(), Ordering::AcqRel);
        if !ptr.is_null() {
            let io_data = CancelIoData { ev_data: ptr };
            io_data.cancel().ok();
        }
    }
}
