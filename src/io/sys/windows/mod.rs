extern crate miow;
extern crate winapi;

mod iocp;
pub mod net;

use std::{io, fmt, ptr};
use std::os::windows::io::AsRawSocket;
use yield_now::get_co_para;
use scheduler::get_scheduler;

pub use self::iocp::{EventData, TimerData, SysEvent, Selector, timeout_handler};

// register the socket to the system selector
#[inline]
pub fn add_socket<T: AsRawSocket + ?Sized>(t: &T) -> io::Result<())> {
    get_scheduler().get_selector().add_socket(t)
}

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
