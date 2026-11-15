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
        let selector = unsafe { self.vec.get_unchecked(id) };
        selector
            .port
            .post(CompletionStatus::new(0, 0, ptr::null_mut()))
            .unwrap();
    }

    /// The shard that owns the socket/handle `raw`.
    ///
    /// Socket registration and io timer registration must agree on this
    /// mapping: the socket's completions are dequeued by this shard's worker,
    /// and its io timers are driven by the same worker's `select`. Socket
    /// handles are 4-byte aligned, so drop the tag bits before spreading them.
    #[inline]
    fn shard_id(&self, raw: usize) -> usize {
        (raw >> 2) % self.vec.len()
    }

    // register file handle to the iocp
    #[inline]
    pub fn add_socket<T: AsRawSocket + ?Sized>(&self, t: &T) -> io::Result<()> {
        // the token para is not used, just pass the handle
        let raw = t.as_raw_socket() as usize;
        let fd = raw >> 2;
        let id = self.shard_id(raw);
        self.vec[id].port.add_socket(fd, t)
    }

    // register the io request to the timeout list
    #[inline]
    #[cfg(feature = "io_timeout")]
    pub fn add_io_timer(&self, io: &Arc<EventData>, timeout: Duration) {
        let id = self.shard_id(io.handle as usize);
        // info!("io timeout = {:?}", dur);
        // the entry keeps its own Arc, so it can run after the request is gone
        let (h, b_new) = self.vec[id].timer_list.add_timer(timeout, io.clone());
        if b_new {
            // wake up the event loop thread of this shard so that it recalls
            // the next wait timeout; another shard's worker cannot make this
            // one recompute its timeout
            self.wakeup(id);
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

#[cfg(all(test, feature = "io_timeout"))]
mod tests {
    use super::*;
    use crate::timeout_list::now;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use windows_sys::Win32::Foundation::WAIT_TIMEOUT;

    const SHARDS: usize = 8;

    /// Arm a zero-duration timer for `raw` and return the selector, the io
    /// data, and the shard `add_socket` maps the handle to.
    fn arm(raw: usize) -> (Selector, Arc<EventData>, usize) {
        let selector = Selector::new(SHARDS).unwrap();
        let data = Arc::new(EventData::new(raw as HANDLE));
        selector.add_io_timer(&data, Duration::ZERO);
        (selector, data, (raw >> 2) % SHARDS)
    }

    /// The shards whose timer list fired the armed timer.
    fn fired_shards(selector: &Selector) -> Vec<usize> {
        let mut fired = Vec::new();
        for id in 0..SHARDS {
            let hit = AtomicBool::new(false);
            let _ = selector.vec[id]
                .timer_list
                .schedule_timer(now(), &|_data: Arc<EventData>| {
                    hit.store(true, Ordering::SeqCst);
                });
            if hit.load(Ordering::SeqCst) {
                fired.push(id);
            }
        }
        fired
    }

    /// The shards that received the `add_io_timer` wakeup packet.
    fn woken_shards(selector: &Selector) -> Vec<usize> {
        let mut woken = Vec::new();
        for id in 0..SHARDS {
            let mut events = [CompletionStatus::zero(); 1];
            match selector.vec[id]
                .port
                .get_many(&mut events, Some(Duration::ZERO))
            {
                Ok(list) => {
                    if list.iter().any(|s| s.overlapped().is_null()) {
                        woken.push(id);
                    }
                }
                Err(ref e) if e.raw_os_error() == Some(WAIT_TIMEOUT as i32) => {}
                Err(e) => panic!("get_many failed: {e}"),
            }
        }
        woken
    }

    /// `add_socket` and `add_io_timer` must file the socket and its timers on
    /// the same shard, and a newly armed timer must wake that shard. The old
    /// code used `(handle % shards) >> 2` for the timer and always woke shard
    /// 0, so a timeout could wait behind a far-away timer of another shard's
    /// worker.
    ///
    /// The handles below cover both ways the old formula disagreed with the
    /// socket mapping: the same shard as expected (420, 428) and a different
    /// one (336, 424, 432).
    #[test]
    fn io_timer_is_armed_on_the_socket_shard_and_wakes_it() {
        for raw in [336usize, 420, 424, 428, 432] {
            let (selector, _data, expected) = arm(raw);
            assert_eq!(
                fired_shards(&selector),
                vec![expected],
                "raw {raw} armed its timer on the wrong shard"
            );
            assert_eq!(
                woken_shards(&selector),
                vec![expected],
                "raw {raw} woke the wrong shard"
            );
        }
    }

    /// A timeout that fires before the request is submitted cannot cancel it:
    /// `CancelIoEx` finds nothing pending. The subscriber must finish the
    /// cancellation after submitting (`cancel_if_timed_out`), otherwise the
    /// request would stay pending forever.
    #[test]
    fn timed_out_before_submission_is_cancelled_after_submission() {
        use super::super::miow::socket_read;
        use std::net::{TcpListener, TcpStream};
        use std::os::windows::io::AsRawSocket;

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let peer = TcpStream::connect(addr).unwrap();
        let (socket, _) = listener.accept().unwrap();
        socket.set_nonblocking(true).unwrap();

        let selector = Selector::new(SHARDS).unwrap();
        selector.add_socket(&socket).unwrap();

        let data = Arc::new(EventData::new(socket.as_raw_socket() as HANDLE));
        let mut buf = [0u8; 1];

        // The handler runs before the request is submitted: its CancelIoEx
        // finds nothing, so only the flag is recorded.
        timeout_handler(data.clone());

        // Finish the submission, then the cancellation.
        let pending =
            unsafe { socket_read(socket.as_raw_socket(), &mut buf, 0, data.get_overlapped()) }
                .unwrap();
        assert!(pending.is_none(), "the read should be pending");
        data.cancel_if_timed_out();

        // With no data and a live peer, the only possible completion is the
        // cancellation. Without the post-submission check none would arrive.
        let shard = selector.shard_id(socket.as_raw_socket() as usize);
        let mut events = [CompletionStatus::zero(); 4];
        let list = match selector.vec[shard]
            .port
            .get_many(&mut events, Some(Duration::from_secs(5)))
        {
            Ok(list) => list.to_vec(),
            Err(ref e) if e.raw_os_error() == Some(WAIT_TIMEOUT as i32) => {
                panic!("no completion arrived after the post-submission cancel")
            }
            Err(e) => panic!("get_many failed: {e}"),
        };
        assert!(
            list.iter().any(|s| s.overlapped() == data.get_overlapped()),
            "the cancellation completion was not delivered to its shard"
        );

        drop(peer);
    }
}
