extern crate miow;
extern crate winapi;
extern crate kernel32;

use std::{cmp, io, u32};
use std::cell::UnsafeCell;
use smallvec::SmallVec;
use self::winapi::*;
use self::miow::Overlapped;
use self::miow::iocp::{CompletionPort, CompletionStatus};
use coroutine::CoroutineImpl;
use queue::spmc::Queue as spmc;
use timeout_list::{TimeoutHandle, ns_to_ms};

type TimerHandle = TimeoutHandle<TimerData>;

// the timeout data
pub struct TimerData {
    handle: HANDLE,
    overlapped: *mut Overlapped,
}

// event associated io data, must be construct in the coroutine
// this passed in to the _overlapped verion API and will read back
// when IOCP get an io event. the timer handle is used to remove
// from the timeout list and co will be pushed to the event_list
// for scheduler
#[repr(C)]
pub struct EventData {
    overlapped: UnsafeCell<Overlapped>,
    handle: HANDLE,
    pub timer: Option<TimerHandle>,
    pub co: Option<CoroutineImpl>,
}

impl EventData {
    pub fn new(handle: HANDLE) -> EventData {
        EventData {
            overlapped: UnsafeCell::new(Overlapped::zero()),
            handle: handle,
            timer: None,
            co: None,
        }
    }

    pub fn get_overlapped(&self) -> &mut Overlapped {
        unsafe { &mut *self.overlapped.get() }
    }

    pub fn timer_data(&self) -> TimerData {
        TimerData {
            handle: self.handle,
            overlapped: self.overlapped.get(),
        }
    }
}

// buffer to receive the system events
pub type Events = SmallVec<[CompletionStatus; 1024]>;

pub struct Selector {
    /// The actual completion port that's used to manage all I/O
    port: CompletionPort,
    /// the event list
    event_list: spmc<CoroutineImpl>,
}

impl Selector {
    pub fn new(workers: usize) -> io::Result<Selector> {
        CompletionPort::new(1).map(|cp| {
            Selector {
                port: cp,
                event_list: spmc::new(workers),
            }
        })
    }

    pub fn select(&self, events: &mut Events, timeout: Option<u64>) -> io::Result<()> {
        let timeout = timeout.map(|to| cmp::min(ns_to_ms(to), u32::MAX as u64) as u32);
        trace!("select; timeout={:?}", timeout);
        trace!("polling IOCP");
        let n = match self.port.get_many(events, timeout) {
            Ok(statuses) => statuses.len(),
            Err(ref e) if e.raw_os_error() == Some(WAIT_TIMEOUT as i32) => 0,
            Err(e) => return Err(e),
        };

        for status in events[..n].iter() {
            // need to check the status for each io
            let overlapped = status.overlapped();
            let data = unsafe { &mut *(overlapped as *mut EventData) };
            let mut co = data.co.take().unwrap();
            let timer_handle = data.timer.take().unwrap();
            trace!("select; -> got overlapped");
            // check the status
            let overlapped = unsafe { &*(*overlapped).raw() };

            trace!("Io stuats = {}", overlapped.Internal);

            match overlapped.Internal as u32 {
                ERROR_OPERATION_ABORTED => {
                    co.set_para(io::Error::new(io::ErrorKind::TimedOut, "timeout"));
                    // it's safe to remove the timer since we are
                    // runing the timer_list in the same thread
                    timer_handle.remove();
                }
                NO_ERROR => {
                    // do nothing here
                }
                err => {
                    co.set_para(io::Error::from_raw_os_error(err as i32));
                }
            }

            // schedule the coroutine
            self.event_list.push(co);
        }

        trace!("returning");
        Ok(())
    }

    // windows register function does nothing,
    // the completion model would call the actuall API instead of register
    #[inline]
    pub fn add_io(&self, _io: &mut EventData) -> io::Result<()> {
        Ok(())
    }

    #[inline]
    // used by the scheduler to pull the coroutine event list
    pub fn get_event_list(&self) -> &spmc<CoroutineImpl> {
        &self.event_list
    }
}

unsafe fn cancel_io(handle: HANDLE, overlapped: &Overlapped) -> io::Result<()> {
    let overlapped = overlapped.raw();
    let ret = kernel32::CancelIoEx(handle, overlapped);
    if ret == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

// when timeout happend we need to cancel the io operation
// this will trigger an event on the IOCP and processed in the selector
pub fn timeout_handler(data: TimerData) {
    unsafe {
        cancel_io(data.handle, &*data.overlapped)
            .map_err(|e| panic!("CancelIoEx failed! e = {}", e))
            .expect("Can't cancel io operation");
    }
}
