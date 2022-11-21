//! coroutine io utilities

#[cfg(unix)]
#[path = "sys/unix/mod.rs"]
pub(crate) mod sys;

#[cfg(windows)]
#[path = "sys/windows/mod.rs"]
pub(crate) mod sys;

// export the generic IO wrapper
pub mod co_io_err;

mod event_loop;
pub(crate) mod thread;

use std::ops::Deref;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) use self::event_loop::EventLoop;
pub use self::sys::co_io::CoIo;
#[cfg(unix)]
pub use self::sys::split_io::{SplitIo, SplitReader, SplitWriter};
#[cfg(unix)]
pub use self::sys::wait_io::WaitIo;
pub(crate) use self::sys::{add_socket, cancel, net, IoData, Selector};

pub trait AsIoData {
    fn as_io_data(&self) -> &IoData;
}

#[derive(Debug)]
pub(crate) struct IoContext {
    nonblocking: AtomicBool,
}

impl IoContext {
    pub fn new() -> Self {
        IoContext {
            // default is blocking mode in coroutine context
            nonblocking: AtomicBool::new(false),
        }
    }

    // return true if it's a nonblocking request
    #[inline]
    pub fn check_nonblocking(&self) -> bool {
        // for coroutine context
        self.nonblocking.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn set_nonblocking(&self, nonblocking: bool) {
        self.nonblocking.store(nonblocking, Ordering::Relaxed);
    }
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
