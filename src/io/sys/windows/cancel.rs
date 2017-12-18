use std::io;
use sync::Mutex;
use cancel::CancelIo;
// use scheduler::get_scheduler;
use super::kernel32;
use super::EventData;

pub struct CancelIoData {
    ev_data: *mut EventData,
}

impl CancelIoData {
    pub fn new(ev_data: &EventData) -> Self {
        CancelIoData { ev_data: ev_data as *const _ as *mut _ }
    }

    pub unsafe fn cancel(&self) -> io::Result<()> {
        let ev = &mut *self.ev_data;
        let handle = ev.handle;
        let overlapped = ev.get_overlapped();
        let ret = kernel32::CancelIoEx(handle, overlapped);
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
