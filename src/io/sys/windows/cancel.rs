use std::io;

use super::EventData;
use crate::cancel::CancelIo;
use crate::sync::AtomicOption;

pub struct CancelIoData {
    ev_data: *mut EventData,
}

unsafe impl Send for CancelIoData {}

impl CancelIoData {
    pub fn new(ev_data: &EventData) -> Self {
        CancelIoData {
            ev_data: ev_data as *const _ as *mut _,
        }
    }

    pub unsafe fn cancel(&self) -> io::Result<()> {
        use windows_sys::Win32::System::IO::CancelIoEx;

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

// windows must use Mutex to protect it's data
// because it will not use the AtomicOption<CoroutineImpl> as a gate keeper
pub struct CancelIoImpl(AtomicOption<CancelIoData>);

impl CancelIo for CancelIoImpl {
    type Data = CancelIoData;

    fn new() -> Self {
        CancelIoImpl(AtomicOption::none())
    }

    fn set(&self, data: CancelIoData) {
        self.0.store(data);
    }

    fn clear(&self) {
        self.0.take();
    }

    unsafe fn cancel(&self) {
        self.0.take().map(|d| d.cancel());
    }
}
