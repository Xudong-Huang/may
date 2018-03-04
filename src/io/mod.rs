//! coroutine io utilities

#[cfg(unix)]
#[path = "sys/unix/mod.rs"]
pub(crate) mod sys;

#[cfg(windows)]
#[path = "sys/windows/mod.rs"]
pub(crate) mod sys;

mod event_loop;

use std::io;
use coroutine_impl::is_coroutine;

pub(crate) use self::event_loop::EventLoop;
pub(crate) use self::sys::{add_socket, cancel, net, IoData, Selector};

pub trait AsIoData {
    fn as_io_data(&self) -> &IoData;
}

#[derive(Debug)]
pub(crate) struct IoContext; 

impl IoContext {
    pub fn new() -> Self {
        IoContext
    }

    #[inline]
    // return Ok(ture) if it's a coroutine context
    pub fn check<F>(&self, f: F) -> io::Result<bool>
    where
        F: FnOnce() -> io::Result<()>,
    {
        if !is_coroutine() {
            f()?;
            return Ok(false);
        }
        Ok(true)
    }
}

// export the generic IO wrapper
pub mod co_io_err;
pub use self::sys::co_io::CoIo;
