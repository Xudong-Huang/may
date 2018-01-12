macro_rules! co_try {
    ($s: expr, $co: expr, $e:expr) => (match $e {
        Ok(val) => val,
        Err(err) => {
            let mut co = $co;
            ::yield_now::set_co_para(&mut co, err);
            $s.schedule(co);
            return;
        }
    })
}

mod iocp;
pub mod net;
pub mod cancel;

use std::{fmt, io};
use std::os::windows::io::AsRawSocket;
use yield_now::get_co_para;
use scheduler::get_scheduler;

pub use self::iocp::{EventData, Selector, SysEvent};

// each file associated data, windows already have OVERLAPPED
// fake windows interface
pub struct IoData;

impl IoData {
    pub fn new<T>(_t: T) -> Self {
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
