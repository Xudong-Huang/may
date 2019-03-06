use std::cell::UnsafeCell;
use std::os::windows::io::{AsRawHandle, AsRawSocket};
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::{io, ptr};

use coroutine_impl::CoroutineImpl;
use miow::iocp::{CompletionPort, CompletionStatus};
use sync::AtomicOption;
use timeout_list::{now, ns_to_dur, TimeOutList, TimeoutHandle};
use winapi::shared::ntdef::*;
use winapi::shared::ntstatus::STATUS_CANCELLED;
use winapi::shared::winerror::*;
use winapi::um::ioapiset::{CancelIoEx, GetOverlappedResult};
use winapi::um::minwinbase::OVERLAPPED;
use yield_now::set_co_para;

// the timeout data
pub struct TimerData {
    event_data: *mut EventData,
}

type TimerList = TimeOutList<TimerData>;
pub type TimerHandle = TimeoutHandle<TimerData>;

// event associated io data, must be construct in the coroutine
// this passed in to the _overlapped version API and will read back
// when IOCP get an io event. the timer handle is used to remove
// from the timeout list and co will be run in the io thread
#[repr(C)]
pub struct EventData {
    overlapped: UnsafeCell<OVERLAPPED>,
    pub handle: HANDLE,
    pub timer: Option<TimerHandle>,
    pub co: AtomicOption<CoroutineImpl>,
}

impl EventData {
    pub fn new(handle: HANDLE) -> EventData {
        EventData {
            overlapped: UnsafeCell::new(unsafe { ::std::mem::zeroed() }),
            handle,
            timer: None,
            co: AtomicOption::none(),
        }
    }

    #[inline]
    pub fn get_overlapped(&mut self) -> *mut OVERLAPPED {
        self.overlapped.get()
    }

    pub fn timer_data(&self) -> TimerData {
        TimerData {
            event_data: self as *const _ as *mut _,
        }
    }

    pub fn get_io_size(&self) -> usize {
        let ol = unsafe { &*self.overlapped.get() };
        ol.InternalHigh as usize
    }
}

// buffer to receive the system events
pub type SysEvent = CompletionStatus;

pub struct Selector {
    /// The actual completion port that's used to manage all I/O
    port: CompletionPort,
    timer_list: TimerList,
    schedule_policy: fn(CoroutineImpl),
}

impl Selector {
    pub fn new(_io_workers: usize, schedule_policy: fn(CoroutineImpl)) -> io::Result<Selector> {
        // only let one thread working, other threads blocking, this is more efficient
        CompletionPort::new(1).map(|cp| Selector {
            port: cp,
            timer_list: TimerList::new(),
            schedule_policy,
        })
    }

    pub fn select(
        &self,
        _id: usize,
        events: &mut [SysEvent],
        timeout: Option<u64>,
    ) -> io::Result<Option<u64>> {
        let timeout = timeout.map(ns_to_dur);
        // info!("select; timeout={:?}", timeout);
        let n = match self.port.get_many(events, timeout) {
            Ok(statuses) => statuses.len(),
            Err(ref e) if e.raw_os_error() == Some(WAIT_TIMEOUT as i32) => 0,
            Err(e) => return Err(e),
        };

        for status in events[..n].iter() {
            // need to check the status for each io
            let overlapped = status.overlapped();
            if overlapped.is_null() {
                // this is just a wakeup event, ignore it
                continue;
            }

            let data = unsafe { &mut *(overlapped as *mut EventData) };
            // when cancel failed the coroutine will continue to finish
            // it's unsafe to ref any local stack value!
            // if cancel not take the coroutine, then it's possible that
            // the coroutine will never come back because there is no event
            let mut co = match data.co.take(Ordering::AcqRel) {
                Some(c) => c,
                // the co may be take in the subscribe
                None => {
                    error!("can't get co in selector");
                    continue;
                }
            };
            co.prefetch();

            // it's safe to remove the timer since we are
            // running the timer_list in the same thread
            // this is not true when running in multi-thread environment
            data.timer.take().map(|h| {
                unsafe {
                    // tell the timer function not to cancel the io
                    // it's not always true that you can really remove the timer entry
                    // it's safe in multi-thread env because it only access its own data
                    h.with_mut_data(|value| value.data.event_data = ptr::null_mut());
                }
                // NOT SAFE for multi-thread!!
                h.remove()
            });

            let overlapped = unsafe { &*overlapped };
            // info!("select got overlapped, status = {}", overlapped.Internal);

            const STATUS_CANCELLED_U32: u32 = STATUS_CANCELLED as u32;
            // check the status
            match overlapped.Internal as u32 {
                ERROR_OPERATION_ABORTED | STATUS_CANCELLED_U32 => {
                    warn!("coroutine timeout, stat=0x{:x}", overlapped.Internal);
                    set_co_para(&mut co, io::Error::new(io::ErrorKind::TimedOut, "timeout"));
                    // timer data is popped already
                }
                NO_ERROR => {
                    // do nothing here
                    // need a way to detect timeout, it's not safe to del timer here
                    // according to windows API it's can't cancel the completed io operation
                    // the timeout function would remove the timer handle
                }
                err => {
                    error!("iocp err=0x{:08x}", err);
                    unsafe {
                        // convert the ntstatus to winerr
                        let mut size: u32 = 0;
                        let o = overlapped as *const _ as *mut _;
                        GetOverlappedResult(data.handle, o, &mut size, i32::from(FALSE));
                    }
                    set_co_para(&mut co, io::Error::last_os_error());
                }
            }

            // schedule the coroutine
            (self.schedule_policy)(co);
        }

        // deal with the timer list
        let next_expire = self.timer_list.schedule_timer(now(), &timeout_handler);
        Ok(next_expire)
    }

    // this will post an os event so that we can wakeup the event loop
    #[inline]
    fn wakeup(&self) {
        // this is not correct for multi thread io, which thread will it wakeup?
        self.port
            .post(CompletionStatus::new(0, 0, ptr::null_mut()))
            .unwrap();
    }

    // register socket handle to the iocp
    #[inline]
    pub fn add_socket<T: AsRawSocket + ?Sized>(&self, t: &T) -> io::Result<()> {
        // the token para is not used, just pass the handle
        self.port.add_socket(t.as_raw_socket() as usize, t)
    }

    // register file handle to the iocp
    #[inline]
    pub fn add_handle<T: AsRawHandle + ?Sized>(&self, t: &T) -> io::Result<()> {
        // the token para is not used, just pass the handle
        self.port.add_handle(t.as_raw_handle() as usize, t)
    }

    // register the io request to the timeout list
    #[inline]
    pub fn add_io_timer(&self, io: &mut EventData, timeout: Option<Duration>) {
        io.timer = timeout.map(|dur| {
            // info!("io timeout = {:?}", dur);
            let (h, b_new) = self.timer_list.add_timer(dur, io.timer_data());
            if b_new {
                // wakeup the event loop thread to recall the next wait timeout
                self.wakeup();
            }
            h
        });
    }
}

unsafe fn cancel_io(handle: HANDLE, overlapped: *mut OVERLAPPED) -> io::Result<()> {
    let ret = CancelIoEx(handle, overlapped);
    if ret == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

// when timeout happened we need to cancel the io operation
// this will trigger an event on the IOCP and processed in the selector
pub fn timeout_handler(data: TimerData) {
    if data.event_data.is_null() {
        return;
    }

    unsafe {
        let event_data = &mut *data.event_data;
        // remove the event timer
        event_data.timer.take();
        // ignore the error, the select may grab the data first!
        cancel_io(event_data.handle, event_data.get_overlapped())
            .unwrap_or_else(|e| error!("CancelIoEx failed! e = {}", e));
    }

    drop(data); // explicitly consume the data
}
