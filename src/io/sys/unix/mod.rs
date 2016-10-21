extern crate nix;
extern crate libc;

#[cfg(target_os = "linux")]
#[path = "epoll.rs"]
mod select;

pub mod net;

use std::io;
use std::os::unix::io::{AsRawFd, RawFd};
use coroutine::CoroutineImpl;
use yield_now::{get_co_para, set_co_para};
use timeout_list::{TimeoutHandle, TimeOutList};

pub use self::select::{SysEvent, Selector};

bitflags! {
    flags EventFlags: u32 {
        const FLAG_READ  = 0b00000001,
        const FLAG_WRITE = 0b00000010,
    }
}

#[inline]
pub fn add_socket<T: AsRawFd + ?Sized>(_s: &T) -> io::Result<()> {
    // get_scheduler().get_selector().add_fd(s.as_raw_fd())
    Ok(())
}

// deal with the io result
#[inline]
fn co_io_result() -> io::Result<()> {
    match get_co_para() {
        Some(err) => {
            return Err(err);
        }
        None => {
            return Ok(());
        }
    }
}

#[inline]
fn from_nix_error(err: nix::Error) -> ::std::io::Error {
    ::std::io::Error::from_raw_os_error(err.errno() as i32)
}

fn timeout_handler(data: TimerData) {
    if data.event_data.is_null() {
        return;
    }

    let event_data = unsafe { &mut *data.event_data };
    // remove the event timer
    event_data.timer.take();

    // get and check the coroutine
    let co = event_data.co.take();
    if co.is_none() {
        // there is no coroutine, just ignore this one
        error!("can't get coroutine in timeout handler");
        return;
    }

    let mut co = co.unwrap();
    set_co_para(&mut co, io::Error::new(io::ErrorKind::TimedOut, "timeout"));

    // resume the coroutine with timeout error
    match co.resume() {
        Some(ev) => ev.subscribe(co),
        None => panic!("coroutine not return!"),
    }
}


// the timeout data
pub struct TimerData {
    event_data: *mut EventData,
}

pub type TimerList = TimeOutList<TimerData>;
pub type TimerHandle = TimeoutHandle<TimerData>;

// event associated io data, must be construct in
// each file handle, the epoll event.data would point to it
pub struct EventData {
    pub fd: RawFd,
    pub interest: EventFlags,
    pub timer: Option<TimerHandle>,
    pub co: Option<CoroutineImpl>,
}

impl EventData {
    pub fn new(fd: RawFd, interest: EventFlags) -> EventData {
        EventData {
            fd: fd,
            interest: interest,
            timer: None,
            co: None,
        }
    }

    pub fn timer_data(&self) -> TimerData {
        TimerData { event_data: self as *const _ as *mut _ }
    }
}
