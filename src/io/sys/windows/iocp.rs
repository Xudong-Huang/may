use std::cell::UnsafeCell;
use std::os::windows::io::AsRawSocket;
use std::time::Duration;
use std::{io, ptr};

use super::miow::{CompletionPort, CompletionStatus};
use crate::coroutine_impl::CoroutineImpl;
use crate::scheduler::Scheduler;
use crate::sync::AtomicOption;
#[cfg(feature = "io_timeout")]
use crate::timeout_list::TimeoutHandle;
use crate::timeout_list::{now, TimeOutList};
use crate::yield_now::set_co_para;
use smallvec::SmallVec;
#[cfg(feature = "io_timeout")]
use std::cell::RefCell;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

// the timeout data
pub struct TimerData {
    event_data: *mut EventData,
}

type TimerList = TimeOutList<TimerData>;
#[cfg(feature = "io_timeout")]
pub type TimerHandle = TimeoutHandle<TimerData>;

// event associated io data, must be construct in the coroutine
// this passed in to the _overlapped version API and will read back
// when IOCP get an io event. the timer handle is used to remove
// from the timeout list and co will be run in the io thread
#[repr(C)]
pub struct EventData {
    overlapped: UnsafeCell<OVERLAPPED>,
    pub handle: HANDLE,
    #[cfg(feature = "io_timeout")]
    pub timer: RefCell<Option<TimerHandle>>,
    pub co: AtomicOption<CoroutineImpl>,
}

// SAFETY: `EventData` is shared between the coroutine (which owns it via an
// `Arc` and only touches it while running/suspended) and the selector thread
// (which mutates `co`/`timer` only after the coroutine has suspended, with the
// IOCP completion providing the happens-before). `handle` is read-only, and
// `co`/`timer` are interior-mutable (`AtomicOption`/`RefCell`). The `overlapped`
// field is only written by the kernel during I/O and read after completion.
unsafe impl Send for EventData {}
unsafe impl Sync for EventData {}

impl EventData {
    pub fn new(handle: HANDLE) -> EventData {
        EventData {
            overlapped: UnsafeCell::new(unsafe { ::std::mem::zeroed() }),
            handle,
            #[cfg(feature = "io_timeout")]
            timer: RefCell::new(None),
            co: AtomicOption::none(),
        }
    }

    #[inline]
    pub fn get_overlapped(&self) -> *mut OVERLAPPED {
        self.overlapped.get()
    }

    #[cfg(feature = "io_timeout")]
    pub fn timer_data(&self) -> TimerData {
        TimerData {
            event_data: self as *const _ as *mut _,
        }
    }

    pub fn get_io_size(&self) -> usize {
        let ol = unsafe { &*self.overlapped.get() };
        ol.InternalHigh
    }
}

// buffer to receive the system events
pub type SysEvent = CompletionStatus;

struct SingleSelector {
    /// The actual completion port that's used to manage all I/O
    port: CompletionPort,
    timer_list: TimerList,
}

impl SingleSelector {
    pub fn new() -> io::Result<SingleSelector> {
        // only let one thread working, other threads blocking, this is more efficient
        CompletionPort::new(1).map(|cp| SingleSelector {
            port: cp,
            timer_list: TimerList::new(),
        })
    }
}

pub(crate) struct Selector {
    // 128 should be fine for max io threads
    vec: SmallVec<[SingleSelector; 128]>,
}

impl Selector {
    pub fn new(io_workers: usize) -> io::Result<Self> {
        let mut s = Selector {
            vec: SmallVec::new(),
        };

        for _ in 0..io_workers {
            let ss = SingleSelector::new()?;
            s.vec.push(ss);
        }

        Ok(s)
    }

    #[inline]
    pub fn select(
        &self,
        scheduler: &Scheduler,
        id: usize,
        events: &mut [SysEvent],
        timeout: Option<u64>,
    ) -> io::Result<Option<u64>> {
        assert!(id < self.vec.len());
        #[cfg(not(feature = "io_timeout"))]
        // Without I/O timers, only a zero timeout is needed to poll the
        // reactor between budgeted local-task drains.
        let timeout = timeout.filter(|to| *to == 0);
        let timeout = timeout.map(Duration::from_nanos);
        // info!("select; timeout={:?}", timeout);
        let single_selector = &self.vec[id];

        let n = match single_selector.port.get_many(events, timeout) {
            Ok(statuses) => statuses.len(),
            Err(ref e) if e.raw_os_error() == Some(WAIT_TIMEOUT as i32) => 0,
            Err(e) => return Err(e),
        };

        for status in unsafe { events.get_unchecked(..n) } {
            // need to check the status for each io
            let overlapped = status.overlapped();
            if overlapped.is_null() {
                // this is just a wakeup event, ignore it
                scheduler.collect_global(id);
                continue;
            }

            let data = unsafe { &*(overlapped as *const EventData) };
            // when cancel failed the coroutine will continue to finish
            // the io data is kept alive by the coroutine (via Arc) until the
            // coroutine resumes, and the cancel path holds its own Arc.
            let mut co = data.co.take().expect("can't get co in selector");

            // the timer list is driven by this same thread
            #[cfg(feature = "io_timeout")]
            data.timer.borrow_mut().take().map(|h| {
                // tell the timer function not to cancel the io
                // it's not always true that you can really remove the timer entry
                h.with_mut_data(|value| value.data.event_data = ptr::null_mut());
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
                    error!("iocp err=0x{err:08x}");
                    unsafe {
                        // convert the ntstatus to winerr
                        let mut size: u32 = 0;
                        let o = overlapped as *const _ as *mut _;
                        GetOverlappedResult(data.handle, o, &mut size, S_FALSE);
                    }
                    set_co_para(&mut co, io::Error::last_os_error());
                }
            }

            #[cfg(feature = "work_steal")]
            scheduler.schedule_with_id(co, id);
            #[cfg(not(feature = "work_steal"))]
            crate::coroutine_impl::run_coroutine(co);
        }

        // Run the local tasks. The drain is budgeted; if it stops with work
        // left, `has_local_tasks` makes the event loop poll again with a zero
        // timeout instead of blocking, so it must stay the last step before
        // returning to `select`.
        scheduler.run_queued_tasks(id);

        // deal with the timer list
        let next_expire = single_selector
            .timer_list
            .schedule_timer(now(), &timeout_handler);
        Ok(next_expire)
    }

    // this will post an os event so that we can wakeup the event loop
    #[inline]
    pub fn wakeup(&self, id: usize) {
        // this is not correct for multi thread io, which thread will it wakeup?
        self.vec[id]
            .port
            .post(CompletionStatus::new(0, 0, ptr::null_mut()))
            .unwrap();
    }

    // register file handle to the iocp
    #[inline]
    pub fn add_socket<T: AsRawSocket + ?Sized>(&self, t: &T) -> io::Result<()> {
        // the token para is not used, just pass the handle
        let fd = (t.as_raw_socket() as usize) >> 2;
        let id = fd % self.vec.len();
        self.vec[id].port.add_socket(fd, t)
    }

    // register the io request to the timeout list
    #[inline]
    #[cfg(feature = "io_timeout")]
    pub fn add_io_timer(&self, io: &EventData, timeout: Duration) {
        let id = (io.handle as usize % self.vec.len()) >> 2;
        // info!("io timeout = {:?}", dur);
        let (h, b_new) = self.vec[id].timer_list.add_timer(timeout, io.timer_data());
        if b_new {
            // wakeup the event loop thread to recall the next wait timeout
            self.wakeup(0);
        }
        io.timer.borrow_mut().replace(h);
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

    let event_data = unsafe { &*data.event_data };
    // remove the event timer
    #[cfg(feature = "io_timeout")]
    event_data.timer.borrow_mut().take();
    // ignore the error, the select may grab the data first!
    let result = unsafe { cancel_io(event_data.handle, event_data.get_overlapped()) };
    result.unwrap_or_else(|e| error!("CancelIoEx failed! e = {e}"));
}
