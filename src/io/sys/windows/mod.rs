macro_rules! co_try {
    ($s:expr, $co:expr, $e:expr) => {
        match $e {
            Ok(val) => val,
            Err(err) => {
                let mut co = $co;
                crate::yield_now::set_co_para(&mut co, err);
                $s.schedule(co);
                return;
            }
        }
    };
}

#[cfg(feature = "io_cancel")]
pub mod cancel;
pub mod co_io;
mod iocp;
pub mod net;
mod pipe;

use std::os::windows::io::AsRawSocket;
use std::sync::atomic::Ordering;
use std::{fmt, io};

use super::thread::ASSOCIATED_IO_RET;
use crate::likely::likely;
use crate::scheduler::get_scheduler;
use crate::yield_now::get_co_para;

pub use self::iocp::{EventData, Selector, SysEvent};

// each file associated data, windows already have OVERLAPPED
// fake windows interface
pub struct IoData;

impl IoData {
    pub fn new<T>(t: T) -> Self {
        drop(t); // unused
        IoData
    }

    // clear the io flag
    pub fn reset(&self) {}
}

impl fmt::Debug for IoData {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, " IoData = {{ ... }}")
    }
}

unsafe impl Send for IoData {}

// register the socket to the system selector
#[inline]
pub fn add_socket<T: AsRawSocket + ?Sized>(t: &T) -> io::Result<IoData> {
    get_scheduler().get_selector().add_socket(t).map(|_| IoData)
}

// deal with the io result
#[inline]
fn co_io_result(io: &EventData, is_coroutine: bool) -> io::Result<usize> {
    if likely(is_coroutine) {
        match get_co_para() {
            None => Ok(io.get_io_size()),
            Some(err) => Err(err),
        }
    } else {
        let err = ASSOCIATED_IO_RET.with(|io_ret| io_ret.take(Ordering::Relaxed));
        match err {
            None => Ok(io.get_io_size()),
            Some(err) => Err(*err),
        }
    }
}
