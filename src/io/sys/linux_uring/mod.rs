//! Completion-based `io_uring` backend for Linux.
//!
//! This tree is selected as `crate::io::sys` when the `io_uring` feature is
//! enabled on Linux, exactly like `sys/unix` and `sys/windows`. It mirrors the
//! Windows IOCP backend: each waiter owns a per-request `EventData`, an adapter
//! submits an SQE, and the worker's ring dispatches the CQE back to the
//! suspended coroutine.

use std::io;
use std::os::fd::AsRawFd;

/// Store the coroutine in `$event_data`, submit `$entry`, and resume the
/// coroutine with the submission error if the ring refuses it.
///
/// Returns a local `Arc<EventData>` for the adapter's post-submission checks.
/// The local clone keeps the request alive even when the coroutine is resumed
/// concurrently and the adapter frame disappears.
macro_rules! submit_co {
    ($s:expr, $event_data:expr, $co:expr, $entry:expr) => {{
        let event_data = $event_data.clone();
        event_data.store_co($co);
        match $s.get_selector().submit(&event_data, $entry) {
            Ok(()) => {}
            Err(err) => {
                let mut co = event_data
                    .take_co()
                    .expect("can't get co in io_uring submit");
                crate::yield_now::set_co_para(&mut co, err);
                $s.schedule(co);
                return;
            }
        }
        event_data
    }};
}

mod event;
mod ring;
mod selector;

#[cfg(feature = "io_cancel")]
pub mod cancel;
pub mod co_io;
pub mod net;
pub mod wait_io;

pub(crate) use self::event::from_nix_error;
pub use self::event::IoData;
pub(crate) use self::ring::SysEvent;
pub(crate) use self::selector::Selector;

/// There is no kernel registration for a socket in completion mode; the fd and
/// the active-request state live in `IoData`.
#[inline]
pub(crate) fn add_socket<T: AsRawFd + ?Sized>(t: &T) -> io::Result<IoData> {
    Ok(IoData::new(t))
}

/// Kept for the readiness API surface (`SplitIo`); completion mode has no
/// per-direction interest mask.
#[inline]
pub(crate) fn mod_socket(_io: &IoData, _is_read: bool) -> io::Result<()> {
    Ok(())
}
