use std::cell::UnsafeCell;
use std::os::windows::io::AsRawSocket;
#[cfg(feature = "io_timeout")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
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
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

/// The timer entry holds the io data of the request it was armed for.
///
/// The `Arc` keeps that `EventData` (and its `OVERLAPPED`) alive even after the
/// request completes and the coroutine drops its reference, so the timeout
/// handler never dereferences freed memory. The io data is created per
/// request, so an entry can only cancel the request it belongs to.
type TimerList = TimeOutList<Arc<EventData>>;
#[cfg(feature = "io_timeout")]
pub type TimerHandle = TimeoutHandle<Arc<EventData>>;

// event associated io data, must be construct in the coroutine
// this passed in to the _overlapped version API and will read back
// when IOCP get an io event. the timer handle is used to remove
// from the timeout list and co will be run in the io thread
#[repr(C)]
pub struct EventData {
    overlapped: UnsafeCell<OVERLAPPED>,
    pub handle: HANDLE,
    #[cfg(feature = "io_timeout")]
    pub timer: AtomicOption<TimerHandle>,
    // set by the io timeout handler when its timer fires. `CancelIoEx` can only
    // cancel a request that is already pending, so a timer that fires before
    // the request is submitted has nothing to cancel; the subscriber re-checks
    // this flag right after the submission and cancels then.
    #[cfg(feature = "io_timeout")]
    timed_out: AtomicBool,
    pub co: AtomicOption<CoroutineImpl>,
}

// Both impls are needed: `HANDLE` is a raw pointer (`*mut c_void`), which is
// neither `Send` nor `Sync` on its own, and `UnsafeCell<OVERLAPPED>` is `!Sync`.
//
// SAFETY: `EventData` is shared between the coroutine (which owns it via an
// `Arc` and only touches it while running/suspended), the selector thread
// (which mutates `co`/`timer` only after the coroutine has suspended, with the
// IOCP completion providing the happens-before), and the io timeout handler
// (which holds its own `Arc` and only touches `timed_out`/`timer`). `handle` is
// read-only, and `co`/`timer`/`timed_out` are interior-mutable atomics. The
// `overlapped` field is only written by the kernel during I/O and read after
// completion.
unsafe impl Send for EventData {}
unsafe impl Sync for EventData {}

impl EventData {
    pub fn new(handle: HANDLE) -> EventData {
        EventData {
            overlapped: UnsafeCell::new(unsafe { ::std::mem::zeroed() }),
            handle,
            #[cfg(feature = "io_timeout")]
            timer: AtomicOption::none(),
            #[cfg(feature = "io_timeout")]
            timed_out: AtomicBool::new(false),
            co: AtomicOption::none(),
        }
    }

    #[inline]
    pub fn get_overlapped(&self) -> *mut OVERLAPPED {
        self.overlapped.get()
    }

    /// Disarm the timer registered for this request, if any.
    ///
    /// The handle is only a best-effort disarm: the timer may already have
    /// fired and taken its entry, in which case `remove` is a no-op.
    #[cfg(feature = "io_timeout")]
    #[inline]
    pub fn take_timer(&self) {
        if let Some(h) = self.timer.take() {
            let _ = h.remove();
        }
    }

    /// Finish a cancellation the timeout handler could not perform.
    ///
    /// The handler marks [`timed_out`](Self::timed_out) before it attempts
    /// `CancelIoEx`. If the request was not submitted yet the cancel fails,
    /// and this call, made right after the successful submission, completes
    /// it while the request is pending.
    #[cfg(feature = "io_timeout")]
    #[inline]
    pub fn cancel_if_timed_out(&self) {
        if self.timed_out.load(Ordering::Acquire) {
            // the handler may have cancelled the request already, in which
            // case this is a no-op (see `cancel_io_on_timeout`)
            unsafe { cancel_io_on_timeout(self.handle, self.get_overlapped()) };
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

            // the timer list is driven by this same thread; the handle is a
            // best-effort disarm, the timer may have taken its entry already
            #[cfg(feature = "io_timeout")]
            data.take_timer();

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
    pub fn add_io_timer(&self, io: &Arc<EventData>, timeout: Duration) {
        let id = (io.handle as usize % self.vec.len()) >> 2;
        // info!("io timeout = {:?}", dur);
        // the entry keeps its own Arc, so it can run after the request is gone
        let (h, b_new) = self.vec[id].timer_list.add_timer(timeout, io.clone());
        if b_new {
            // wakeup the event loop thread to recall the next wait timeout
            self.wakeup(0);
        }
        io.timer.store(h);
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

/// Attempt the cancel of a timed-out request, ignoring a request that is no
/// longer pending.
///
/// The timeout handler and the subscriber can both call this for the same
/// request: the handler may run before the request is submitted, in which case
/// the subscriber completes the cancel after the submission. `CancelIoEx`
/// cancels exactly the operation identified by the `OVERLAPPED` pointer, so a
/// second attempt against an already canceled or completed request finds
/// nothing and fails with `ERROR_NOT_FOUND`; that is expected, not an error.
unsafe fn cancel_io_on_timeout(handle: HANDLE, overlapped: *mut OVERLAPPED) {
    if let Err(e) = unsafe { cancel_io(handle, overlapped) } {
        if e.raw_os_error() != Some(ERROR_NOT_FOUND as i32) {
            error!("CancelIoEx failed! e = {e}");
        }
    }
}

// when timeout happened we need to cancel the io operation
// this will trigger an event on the IOCP and processed in the selector
pub fn timeout_handler(event_data: Arc<EventData>) {
    // Mark the request as timed out before attempting the cancel. If it is not
    // submitted yet `CancelIoEx` finds nothing; the subscriber completes the
    // cancellation right after submitting, see `cancel_if_timed_out`.
    #[cfg(feature = "io_timeout")]
    event_data.timed_out.store(true, Ordering::Release);
    // remove the event timer
    #[cfg(feature = "io_timeout")]
    event_data.take_timer();
    // the request may not be submitted yet (the subscriber retries after the
    // submission) or may have completed already: a missing request is fine
    unsafe { cancel_io_on_timeout(event_data.handle, event_data.get_overlapped()) };
}
