#[cfg(unix)]
#[path = "sys/unix/mod.rs"]
mod sys;

#[cfg(windows)]
#[path = "sys/windows/mod.rs"]
mod sys;

mod co_io;
mod event_loop;

use std::io;
use coroutine_impl::is_coroutine;

#[cfg(unix)]
pub(crate) use self::sys::del_socket;
pub(crate) use self::event_loop::EventLoop;
pub(crate) use self::sys::{add_socket, cancel, net, IoData, Selector};

pub use self::co_io::{AsRaw, CoIO};


pub trait AsIoData {
    fn as_io_data(&self) -> &IoData;
}

#[derive(Debug)]
pub(crate) struct IoContext {
    b_init: bool,
    b_co: bool,
}

impl IoContext {
    pub fn new() -> Self {
        IoContext {
            b_init: false,
            b_co: true,
        }
    }

    #[inline]
    pub fn check<F>(&self, f: F) -> io::Result<bool>
    where
        F: FnOnce() -> io::Result<()>,
    {
        if !self.b_init {
            let me = unsafe { &mut *(self as *const _ as *mut Self) };
            if !is_coroutine() {
                me.b_co = false;
                f()?;
            }
            me.b_init = true;
        }
        Ok(self.b_co)
    }
}
