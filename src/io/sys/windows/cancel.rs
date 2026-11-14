use std::io;
use std::sync::Arc;

use super::EventData;
use crate::cancel::CancelIo;
use crate::sync::AtomicOption;

// Keeps the pending `EventData` alive via `Arc` so that a concurrent cancel can
// call `CancelIoEx` without dereferencing the target coroutine's stack frame.
pub struct CancelIoData {
    ev_data: Arc<EventData>,
}

impl CancelIoData {
    pub fn new(ev_data: &Arc<EventData>) -> Self {
        CancelIoData {
            ev_data: ev_data.clone(),
        }
    }

    pub fn cancel(&self) -> io::Result<()> {
        use windows_sys::Win32::System::IO::CancelIoEx;

        let ev = self.ev_data.as_ref();
        let handle = ev.handle;
        let overlapped = ev.get_overlapped();
        let ret = unsafe { CancelIoEx(handle, overlapped) };
        if ret == 0 {
            let err = io::Error::last_os_error();
            error!("cancel err={err:?}");
            // ev.co.take().map(|co| get_scheduler().schedule(co));
            Err(err)
        } else {
            Ok(())
        }
    }
}

// windows keeps the io data alive via `Arc<EventData>`, so unlike unix it
// doesn't need `AtomicOption<CoroutineImpl>` as a gate keeper for the cancel
// path.
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

    fn cancel(&self) -> bool {
        // A successful `CancelIoEx` means an IOCP completion will arrive and
        // the selector will resume the coroutine, so the cancel is handled.
        // On error fall back to the coroutine-based cancel path.
        self.0.take().is_some_and(|d| d.cancel().is_ok())
    }
}
