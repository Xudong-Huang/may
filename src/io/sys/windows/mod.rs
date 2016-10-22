extern crate miow;
extern crate winapi;

mod iocp;
pub mod net;

use std::{io, fmt, ptr};
use std::os::windows::io::AsRawSocket;
use yield_now::get_co_para;
use scheduler::get_scheduler;

pub use self::iocp::{EventData, SysEvent, Selector};

// each file associated data, windows already have OVERLAPPED
// fake windows interface
pub struct IoData;

impl IoData {
    pub fn new<T>(_t: T) -> Self {
        IoData
    }

    #[inline]
    pub fn inner(&self) -> &mut EventData {
        unsafe { &mut *ptr::null_mut() }
    }

    // clear the io flag
    #[inline]
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

#[allow(dead_code)]
pub fn del_socket(_io: &IoData) {}

// deal with the io result
#[inline]
fn co_io_result(io: &EventData) -> io::Result<usize> {
    match get_co_para() {
        Some(err) => {
            return Err(err);
        }
        None => {
            return Ok(io.get_io_size());
        }
    }
}