#[cfg(any(target_os = "linux", target_os = "android"))]
#[path = "epoll.rs"]
mod select;

#[cfg(any(
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "ios",
    target_os = "macos",
    target_os = "netbsd",
    target_os = "openbsd"
))]
#[path = "kqueue.rs"]
mod select;

#[cfg(feature = "io_cancel")]
pub mod cancel;
pub mod co_io;
pub mod net;
pub mod wait_io;

use std::ops::Deref;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::io::{AsRawFd, RawFd};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
#[cfg(feature = "io_timeout")]
use std::time::Duration;
use std::{fmt, io};

use crate::coroutine_impl::{run_coroutine, CoroutineImpl};
use crate::io::thread::ASSOCIATED_IO_RET;
use crate::likely::likely;
use crate::scheduler::get_scheduler;
use crate::sync::{AtomicOption, WaitCo};
#[cfg(feature = "io_timeout")]
use crate::timeout_list::{TimeOutList, TimeoutHandle};
use crate::yield_now::get_co_para;
#[cfg(feature = "io_timeout")]
use crate::yield_now::set_co_para;

pub(crate) use self::select::{Selector, SysEvent};

#[inline]
pub fn add_socket<T: AsRawFd + ?Sized>(t: &T) -> io::Result<IoData> {
    get_scheduler().get_selector().add_fd(IoData::new(t))
}

#[inline]
pub fn mod_socket(io: &IoData, is_read: bool) -> io::Result<()> {
    get_scheduler().get_selector().mod_fd(io, is_read)
}

#[inline]
fn del_socket(io: &IoData) {
    // transfer the io to the selector
    get_scheduler().get_selector().del_fd(io);
}

// deal with the io result
#[inline]
fn co_io_result(is_coroutine: bool) -> io::Result<()> {
    if likely(is_coroutine) {
        match get_co_para() {
            None => Ok(()),
            Some(err) => Err(err),
        }
    } else {
        let err = ASSOCIATED_IO_RET.with(|io_ret| io_ret.take());
        match err {
            None => Ok(()),
            Some(err) => Err(*err),
        }
    }
}

#[inline]
fn from_nix_error(err: nix::Error) -> ::std::io::Error {
    std::io::Error::from_raw_os_error(err as i32)
}

/// Take the coroutine of the wait this timer was armed for.
///
/// The timer entry owns a clone of that wait's shared slot, so it can safely
/// run even after the io data is gone; when the wait already ended the slot
/// is empty and there is nothing to do. Every wait gets a fresh slot, so an
/// entry left over from an earlier wait can only look into its own emptied
/// slot and never reach a later wait.
#[cfg(feature = "io_timeout")]
fn timeout_handler(wait: WaitCo) {
    let mut co = match wait.take() {
        Some(co) => co,
        None => return,
    };

    set_co_para(&mut co, io::Error::new(io::ErrorKind::TimedOut, "timeout"));

    // resume the coroutine with timeout error
    run_coroutine(co);
}

#[cfg(feature = "io_timeout")]
pub type TimerList = TimeOutList<WaitCo>;
#[cfg(feature = "io_timeout")]
pub type TimerHandle = TimeoutHandle<WaitCo>;

// event associated io data, must be construct in
// each file handle, the epoll event.data would point to it
pub struct EventData {
    pub fd: RawFd,
    pub io_flag: AtomicUsize,
    #[cfg(feature = "io_timeout")]
    pub timer: AtomicOption<TimerHandle>,
    // A fresh shared slot is published for every wait. The selector, a
    // cancellation and the timer armed for that wait all take the coroutine
    // from this same slot, so exactly one of them can win, and a stale timer
    // entry keeps a claim on its own (already emptied) slot instead of
    // reaching a later wait.
    pub wait: AtomicOption<WaitCo>,
}

impl EventData {
    pub fn new(fd: RawFd) -> EventData {
        EventData {
            fd,
            io_flag: AtomicUsize::new(0),
            #[cfg(feature = "io_timeout")]
            timer: AtomicOption::none(),
            wait: AtomicOption::none(),
        }
    }

    /// Disarm the timer registered for this io request, if any.
    ///
    /// The entry stays linked if it is the newest node (or if the consumer
    /// already took it); the wait it belongs to is then already emptied, so a
    /// later fire only finds an empty slot and does nothing.
    #[cfg(feature = "io_timeout")]
    #[inline]
    pub fn take_timer(&self) {
        if let Some(h) = self.timer.take() {
            let _ = h.remove();
        }
    }

    #[inline]
    pub fn schedule(&self) {
        let wait = match self.wait.take() {
            Some(wait) => wait,
            None => return, // it's already taken by the selector
        };
        let co = match wait.take() {
            Some(co) => co,
            None => return, // it's already taken by the selector or the timer
        };

        // it's safe to remove the timer since we are running the timer_list in the same thread
        #[cfg(feature = "io_timeout")]
        self.take_timer();

        // schedule the coroutine
        get_scheduler().schedule(co);
    }

    /// used by local re-schedule that in `subscribe`
    #[inline]
    pub fn fast_schedule(&self, wait: &WaitCo) {
        let co = match wait.take() {
            Some(co) => co,
            None => return, // it's already taken by the selector or the timer
        };

        // it's safe to remove the timer since we are running the timer_list in the same thread
        #[cfg(feature = "io_timeout")]
        self.take_timer();

        // run the coroutine
        run_coroutine(co);
    }
}

// each file associated data
pub struct IoData {
    event_data: Arc<EventData>,
    // whether the fd has been successfully added to the selector
    registered: bool,
}

impl IoData {
    pub fn new<T: AsRawFd + ?Sized>(t: &T) -> Self {
        let fd = t.as_raw_fd();
        let event_data = Arc::new(EventData::new(fd));
        IoData {
            event_data,
            registered: false,
        }
    }

    /// Mark this io data as successfully registered with the selector.
    ///
    /// Only a registered `IoData` may run the deregistration in `Drop`: after
    /// a failed registration attempt there is nothing to remove, and the DEL
    /// would remove the registration of another owner of the same fd. That is
    /// exactly what happens on `EEXIST`, returned when a second `CoIo` wraps
    /// an fd that is already registered.
    #[inline]
    fn mark_registered(&mut self) {
        self.registered = true;
    }

    /// Publish `co` as the waiter for this io data and arm its timeout.
    ///
    /// The coroutine goes into a fresh shared slot that the selector, a
    /// cancellation and the timer armed for this wait all draw from, so exactly
    /// one of them can resume it. The slot is published to the io data before
    /// the timer is armed: a zero-duration timer that fires while it is being
    /// armed still finds the coroutine, otherwise the timeout would be dropped
    /// and the wait parked forever.
    ///
    /// The wait is stored before the timer is armed so that a publish can never
    /// overwrite a later one: a later wait can only start after this coroutine
    /// has been resumed on another thread, which requires this store to be
    /// visible already. From that point on only the returned shared event data
    /// may be used, never `self`: the resumed coroutine may have moved or
    /// dropped the `IoData` this was called on.
    #[inline]
    pub(crate) fn publish_co(
        &self,
        co: CoroutineImpl,
        #[cfg(feature = "io_timeout")] timeout: Option<Duration>,
    ) -> (Arc<EventData>, WaitCo) {
        let event_data = self.event_data.clone();
        let wait = Arc::new(AtomicOption::some(co));

        // the previous wait always ended before a new one starts, so its timer
        // only needs disarming. Do it before publishing, while no publish of a
        // later wait for this io data can exist yet.
        #[cfg(feature = "io_timeout")]
        event_data.take_timer();

        event_data.wait.store(wait.clone());

        #[cfg(feature = "io_timeout")]
        if let Some(dur) = timeout {
            get_scheduler()
                .get_selector()
                .add_io_timer(&event_data, dur, wait.clone());
        }

        (event_data, wait)
    }

    // clear the io flag
    #[inline]
    pub fn reset(&self) -> usize {
        self.event_data.io_flag.swap(0, Ordering::AcqRel)
    }
}

impl Deref for IoData {
    type Target = Arc<EventData>;

    fn deref(&self) -> &Arc<EventData> {
        &self.event_data
    }
}

impl fmt::Debug for IoData {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "IoData = {{ ... }}")
    }
}

impl Drop for IoData {
    fn drop(&mut self) {
        // an fd that was never registered has nothing to remove; issuing the
        // DEL anyway could deregister a live registration of the same fd
        // (e.g. after `EEXIST` from a second `CoIo` over the same fd)
        if self.registered {
            del_socket(self);
        }
    }
}

impl AsFd for IoData {
    fn as_fd(&self) -> BorrowedFd<'_> {
        unsafe { BorrowedFd::borrow_raw(self.event_data.fd) }
    }
}

unsafe impl Send for IoData {}

#[cfg(all(test, feature = "io_timeout"))]
mod tests {
    use super::*;
    use crate::coroutine_impl::EventSource;
    use crate::yield_now::yield_with_io;
    use std::sync::mpsc::{channel, Sender};
    use std::thread;

    struct Capture(Sender<CoroutineImpl>);

    impl EventSource for Capture {
        fn subscribe(&mut self, co: CoroutineImpl) {
            self.0.send(co).unwrap();
        }
    }

    /// A zero-duration timer must not lose the timeout when it expires before
    /// the wait has been published through the io data.
    ///
    /// The timer owns its own clone of the wait slot, so the interleaving is
    /// driven deterministically here: arm the timer with the slot, let the io
    /// worker fire it, and only then publish the (now emptied) slot.
    #[test]
    fn zero_timeout_is_not_lost_before_the_wait_is_published() {
        let socket = std::net::UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let io = IoData::new(&socket);

        let (tx_co, rx_co) = channel();
        let (tx_done, rx_done) = channel();
        crate::go!(move || {
            let capture = Capture(tx_co);
            yield_with_io(&capture, true);
            tx_done.send(()).unwrap();
        });

        let co = rx_co
            .recv_timeout(Duration::from_secs(5))
            .expect("the coroutine did not suspend");

        let wait = Arc::new(AtomicOption::some(co));
        get_scheduler()
            .get_selector()
            .add_io_timer(&io, Duration::ZERO, wait.clone());
        // let the io worker pop the already expired entry; the timer carries
        // the wait, so the coroutine can be taken before the slot is published
        thread::sleep(Duration::from_millis(50));
        io.event_data.wait.store(wait);

        assert!(
            rx_done.recv_timeout(Duration::from_secs(5)).is_ok(),
            "the zero timeout was dropped before the wait was published"
        );
    }

    /// The production publish path must deliver a zero-duration timeout even
    /// though it expires immediately.
    #[test]
    fn zero_timeout_through_publish_is_delivered() {
        let socket = std::net::UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let io = IoData::new(&socket);

        let (tx_co, rx_co) = channel();
        let (tx_done, rx_done) = channel();
        crate::go!(move || {
            let capture = Capture(tx_co);
            yield_with_io(&capture, true);
            tx_done.send(()).unwrap();
        });

        let co = rx_co
            .recv_timeout(Duration::from_secs(5))
            .expect("the coroutine did not suspend");

        let (_, wait) = io.publish_co(co, Some(Duration::ZERO));

        assert!(
            rx_done.recv_timeout(Duration::from_secs(5)).is_ok(),
            "the zero timeout through publish_co was dropped"
        );
        // the timer must have taken the coroutine out of the shared slot
        assert!(wait.take().is_none());
    }
}
