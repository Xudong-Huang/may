use std::io;
use sync::Mutex;
use cancel::CancelIo;
use super::kernel32;
use super::winapi::*;
use super::EventData;

pub struct CancelIoData {
    handle: HANDLE,
    overlapped: *mut OVERLAPPED,
}

impl CancelIoData {
    pub fn new(ev_data: &EventData) -> Self {
        CancelIoData {
            handle: ev_data.handle,
            overlapped: ev_data.get_overlapped().raw(),
        }
    }

    pub unsafe fn cancel(&self) -> io::Result<()> {
        let ret = kernel32::CancelIoEx(self.handle, self.overlapped);
        if ret == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

// windows must use Mutex to protect it's data
// because it will not use the AtomicOption<CoroutineImpl> as a gate keeper
pub struct CancelIoImpl(Mutex<Option<CancelIoData>>);

impl CancelIo for CancelIoImpl {
    type Data = CancelIoData;

    fn new() -> Self {
        CancelIoImpl(Mutex::new(None))
    }

    fn set(&self, data: CancelIoData) {
        *self.0.lock().expect("failed to get CancelIo lock") = Some(data);
    }

    fn clear(&self) {
        *self.0.lock().expect("failed to get CancelIo lock") = None;
    }

    unsafe fn cancel(&self) {
        self.0.lock().expect("failed to get CancelIo lock").take().map(|d| d.cancel());
    }
}
