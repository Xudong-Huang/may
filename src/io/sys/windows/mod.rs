extern crate miow;
extern crate winapi;

mod iocp;
pub mod net;

use std::io;
use std::os::windows::io::AsRawSocket;
use super::EventData;
use yield_now::get_co_para;
use scheduler::get_scheduler;

pub use self::iocp::{EventData, TimerData, SysEvent, Selector, timeout_handler};

// each file associated data, windows already have OVERLAPPED
pub struct IoData();

impl IoData {
    pub fn new() -> Self {
        IoData()
    }
}

// register the socket to the system selector
#[inline]
pub fn add_socket<T: AsRawSocket + ?Sized>(t: &T) -> io::Result<IoData> {
    let s = get_scheduler();
    s.get_selector().add_socket(t).map(|_| IoData())
}

// fake windows interface
#[inline]
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
