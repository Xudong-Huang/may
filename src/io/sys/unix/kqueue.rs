use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::io::OwnedFd;
use std::time::Duration;
use std::{io, ptr};

use super::select_loop::{Ready, ShardState};
use super::{EventData, IoData};
use crate::scheduler::Scheduler;
#[cfg(feature = "io_timeout")]
use crate::sync::WaitCo;

use smallvec::SmallVec;

pub type SysEvent = libc::kevent;

// used for notify wakeup
const NOTIFY_IDENT: usize = 42;

macro_rules! kevent {
    ($id:expr, $filter:expr, $flags:expr, $data:expr) => {
        libc::kevent {
            ident: $id as libc::uintptr_t,
            filter: $filter,
            flags: $flags,
            fflags: 0,
            data: 0,
            udata: $data as *mut _,
        }
    };
}

macro_rules! syscall {
    ($fn: ident ( $($arg: expr),* $(,)* ) ) => {{
        #[allow(unused_unsafe)]
        let res = unsafe { libc::$fn($($arg, )*) };
        if res < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(res)
        }
    }};
}

struct SingleSelector {
    kqfd: OwnedFd,
    state: ShardState,
}

impl AsRawFd for SingleSelector {
    fn as_raw_fd(&self) -> RawFd {
        self.kqfd.as_raw_fd()
    }
}

impl SingleSelector {
    pub fn new() -> io::Result<Self> {
        let kqfd = unsafe { OwnedFd::from_raw_fd(libc::kqueue()) };
        syscall!(fcntl(kqfd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC))?;

        let mut kev = libc::kevent {
            ident: NOTIFY_IDENT,
            filter: libc::EVFILT_USER,
            flags: libc::EV_ADD | libc::EV_CLEAR | libc::EV_RECEIPT,
            fflags: 0,
            data: 0,
            udata: ptr::null_mut(),
        };

        syscall!(kevent(kqfd.as_raw_fd(), &kev, 1, &mut kev, 1, ptr::null()))?;
        if kev.flags & libc::EV_ERROR != 0 && kev.data != 0 {
            return Err(io::Error::from_raw_os_error(kev.data as i32));
        }

        Ok(SingleSelector {
            kqfd,
            state: ShardState::new(),
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
    fn shard_id(&self, fd: i32) -> usize {
        fd as usize % self.vec.len()
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
        let single_selector = &self.vec[id];
        let kqfd = single_selector.as_raw_fd();

        single_selector.state.select(
            scheduler,
            id,
            events,
            timeout,
            |remaining, events| {
                let timeout_spec = remaining.map(|to| {
                    let dur = Duration::from_nanos(to);
                    libc::timespec {
                        tv_sec: dur.as_secs() as libc::time_t,
                        tv_nsec: dur.subsec_nanos() as libc::c_long,
                    }
                });
                let timeout = timeout_spec
                    .as_ref()
                    .map(|s| s as *const _)
                    .unwrap_or(ptr::null());
                // debug!("select; timeout={:?}", timeout_spec);

                // Wait for kqueue events for at most timeout_spec
                syscall!(kevent(
                    kqfd,
                    ptr::null(),
                    0,
                    events.as_mut_ptr(),
                    events.len() as libc::c_int,
                    timeout,
                ))
                .map(|n| n as usize)
            },
            |event| {
                if event.ident == NOTIFY_IDENT && event.filter == libc::EVFILT_USER {
                    Ready::Wakeup
                } else {
                    // the event data is shared through `Arc`, so only a shared
                    // reference may be created here; everything touched on it is
                    // interior-mutable (atomics / `AtomicOption`)
                    let data = unsafe { &*(event.udata as *const EventData) };
                    // info!("select got event, data={:p}", data);
                    Ready::Io(data, event.flags as usize)
                }
            },
        )
    }

    // this will post an os event so that we can wakeup the event loop
    #[inline]
    pub fn wakeup(&self, id: usize) {
        let selector = &self.vec[id];
        let kqfd = selector.as_raw_fd();
        let mut kev = libc::kevent {
            ident: NOTIFY_IDENT,
            filter: libc::EVFILT_USER,
            flags: libc::EV_ADD | libc::EV_RECEIPT,
            fflags: libc::NOTE_TRIGGER,
            data: 0,
            udata: ptr::null_mut(),
        };

        syscall!(kevent(kqfd, &kev, 1, &mut kev, 1, ptr::null())).unwrap();
        assert!(kev.flags & libc::EV_ERROR == 0 || kev.data == 0);

        trace!("wakeup id={:?}", id);
    }

    // register io event to the selector
    #[inline]
    pub fn add_fd(&self, mut io_data: IoData) -> io::Result<IoData> {
        let fd = io_data.fd;
        let id = self.shard_id(fd);
        let kqfd = self.vec[id].as_raw_fd();

        let flags = libc::EV_ADD | libc::EV_CLEAR | libc::EV_RECEIPT;
        let udata = io_data.as_ref() as *const _;
        let mut changes = [
            kevent!(fd, libc::EVFILT_READ, flags, udata),
            kevent!(fd, libc::EVFILT_WRITE, flags, udata),
        ];

        // Only mark the io data as registered after a successful submission.
        // If this fails, dropping the io data must not issue a DEL: it would
        // remove any pre-existing filters of the same fd.
        syscall!(kevent(
            kqfd,
            changes.as_ptr(),
            changes.len() as libc::c_int,
            changes.as_mut_ptr(),
            changes.len() as libc::c_int,
            ptr::null(),
        ))?;
        io_data.mark_registered();

        debug!("add fd to kqueue select, fd={:?}", fd);
        Ok(io_data)
    }

    #[inline]
    pub fn mod_fd(&self, io_data: &IoData, is_read: bool) -> io::Result<()> {
        let fd = io_data.fd;
        let id = self.shard_id(fd);
        let kqfd = self.vec[id].as_raw_fd();

        let flags = libc::EV_CLEAR | libc::EV_RECEIPT;
        let udata = io_data.as_ref() as *const _;
        let mut changes = if is_read {
            [
                kevent!(fd, libc::EVFILT_WRITE, flags | libc::EV_DELETE, udata),
                kevent!(fd, libc::EVFILT_READ, flags | libc::EV_ADD, udata),
            ]
        } else {
            [
                kevent!(fd, libc::EVFILT_WRITE, flags | libc::EV_ADD, udata),
                kevent!(fd, libc::EVFILT_READ, flags | libc::EV_DELETE, udata),
            ]
        };

        syscall!(kevent(
            kqfd,
            changes.as_ptr(),
            changes.len() as libc::c_int,
            changes.as_mut_ptr(),
            changes.len() as libc::c_int,
            ptr::null(),
        ))?;

        debug!("modify fd to kqueue select, fd={:?}", fd);
        Ok(())
    }

    #[inline]
    pub fn del_fd(&self, io_data: &IoData) {
        // mark the timer as removed if any, this only happened when cancel
        // an IO. what if the timer expired at the same time? because we run
        // this func in the user space, so the timer handler will not got the
        // coroutine
        #[cfg(feature = "io_timeout")]
        io_data.take_timer();

        let fd = io_data.fd;
        let id = self.shard_id(fd);
        let single_selector = &self.vec[id];
        let kqfd = single_selector.as_raw_fd();

        let filter = libc::EV_DELETE | libc::EV_RECEIPT;
        let mut changes = [
            kevent!(fd, libc::EVFILT_READ, filter, ptr::null_mut()),
            kevent!(fd, libc::EVFILT_WRITE, filter, ptr::null_mut()),
        ];

        // ignore the error, the fd may already closed
        syscall!(kevent(
            kqfd,
            changes.as_ptr(),
            changes.len() as libc::c_int,
            changes.as_mut_ptr(),
            changes.len() as libc::c_int,
            ptr::null(),
        ))
        .ok();

        debug!("del fd from kqueue select, fd={:?}", fd);
        // hand the event data to the owning event loop for release, waking it
        // off the batch's first push, see `ShardState::discard`
        single_selector.state.discard(io_data, || self.wakeup(id));
    }

    // register the io request to the timeout list
    #[inline]
    #[cfg(feature = "io_timeout")]
    pub fn add_io_timer(&self, io: &EventData, timeout: Duration, wait: WaitCo) {
        let id = self.shard_id(io.fd);
        // info!("io timeout = {:?}", dur);
        let (h, b_new) = self.vec[id].state.timer_list.add_timer(timeout, wait);
        if b_new {
            // wakeup the event loop thread to recall the next wait timeout
            self.wakeup(id);
        }
        // The wait is already published, so it may be resumed while it is
        // being armed (a zero-duration timer or an io event). The handle is
        // kept only as a best-effort disarm for this wait: a concurrent
        // publish may replace it, in which case the disarmed entry belongs to
        // a different wait. That is harmless because every entry owns the
        // (emptied) slot of the wait it was armed for.
        io.timer.store(h);
    }
}
