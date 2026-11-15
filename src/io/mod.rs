//! coroutine io utilities
//!
//! An io object belongs to one coroutine: do not share it across threads or
//! coroutines, and never wait on it from more than one coroutine at a time.
//! Only the most recent wait of an object is kept, so a concurrent wait would
//! leave the earlier coroutine parked forever; the library detects that misuse
//! and fails the earlier wait with an io error, so its coroutine can handle it
//! and continue.

#[cfg(unix)]
#[path = "sys/unix/mod.rs"]
pub(crate) mod sys;

#[cfg(windows)]
#[path = "sys/windows/mod.rs"]
pub(crate) mod sys;

// export the generic IO wrapper
pub mod co_io_err;

mod event_loop;
pub(crate) mod split_io;
pub(crate) mod thread;

use std::ops::Deref;

pub(crate) use self::event_loop::EventLoop;
#[cfg(feature = "io_cancel")]
pub(crate) use self::sys::cancel;
pub use self::sys::co_io::CoIo;
#[cfg(unix)]
pub use self::sys::wait_io::{WaitIo, WaitIoWaker};
pub use self::sys::IoData;
pub(crate) use self::sys::{add_socket, net, Selector};
pub use split_io::{SplitIo, SplitReader, SplitWriter};

/// An io object that owns its coroutine io data.
///
/// The object belongs to one coroutine. Do not share it across threads or
/// coroutines for concurrent waits: only the most recent wait of an object is
/// kept, so the library detects a concurrent wait and fails the earlier waiter
/// with an io error.
pub trait AsIoData {
    fn as_io_data(&self) -> &IoData;
}

// an option type that implement deref
struct OptionCell<T>(Option<T>);

impl<T> Deref for OptionCell<T> {
    type Target = T;
    fn deref(&self) -> &T {
        match self.0.as_ref() {
            Some(d) => d,
            None => panic!("no data to deref for OptionCell"),
        }
    }
}

impl<T> OptionCell<T> {
    pub fn new(data: T) -> Self {
        OptionCell(Some(data))
    }

    pub fn take(&mut self) -> T {
        match self.0.take() {
            Some(d) => d,
            None => panic!("no data to take for OptionCell"),
        }
    }
}
