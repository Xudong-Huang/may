use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::io::OwnedFd;
use std::sync::atomic::Ordering;
use std::sync::Arc;
#[cfg(feature = "io_timeout")]
use std::time::Duration;
use std::{io, ptr};

#[cfg(feature = "io_timeout")]
use super::{timeout_handler, TimerList};
use super::{EventData, IoData};
use crate::scheduler::Scheduler;
#[cfg(feature = "io_timeout")]
use crate::timeout_list::now;

use may_queue::mpsc::Queue;
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

pub(crate) struct SingleSelector {
    kqfd: OwnedFd,
    #[cfg(feature = "io_timeout")]
    timer_list: TimerList,
    free_ev: Queue<Arc<EventData>>,
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
            free_ev: Queue::new(),
            #[cfg(feature = "io_timeout")]
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
        _timeout: Option<u64>,
    ) -> io::Result<Option<u64>> {
        #[cfg(feature = "io_timeout")]
        let timeout_spec = _timeout.map(|to| {
            let dur = Duration::from_nanos(to);
            libc::timespec {
                tv_sec: dur.as_secs() as libc::time_t,
                tv_nsec: dur.subsec_nanos() as libc::c_long,
            }
        });

        #[cfg(feature = "io_timeout")]
        let timeout = timeout_spec
            .as_ref()
            .map(|s| s as *const _)
            .unwrap_or(ptr::null());
        #[cfg(not(feature = "io_timeout"))]
        let timeout = ptr::null();
        // debug!("select; timeout={:?}", timeout_spec);

        let single_selector = unsafe { self.vec.get_unchecked(id) };

        // Wait for kqueue events for at most timeout_ms milliseconds
        let kqfd = single_selector.as_raw_fd();
        let n = syscall!(kevent(
            kqfd,
            ptr::null(),
            0,
            events.as_mut_ptr(),
            events.len() as libc::c_int,
            timeout,
        ))?;

        let n = n as usize;

        for event in unsafe { events.get_unchecked(..n) } {
            if event.ident == NOTIFY_IDENT && event.filter == libc::EVFILT_USER {
                scheduler.collect_global(id);
                continue;
            }
            let data = unsafe { &mut *(event.udata as *mut EventData) };
            // info!("select got event, data={:p}", data);
            data.io_flag
                .fetch_or(event.flags as usize, Ordering::Release);

            // first check the atomic co, this may be grab by the worker first
            let co = match data.co.take() {
                None => continue,
                Some(co) => co,
            };

            // it's safe to remove the timer since we are running the timer_list in the same thread
            #[cfg(feature = "io_timeout")]
            data.timer.borrow_mut().take().map(|h| {
                unsafe {
                    // tell the timer handler not to cancel the io
                    // it's not always true that you can really remove the timer entry
                    h.with_mut_data(|value| value.data.event_data = ptr::null_mut());
                }
                h.remove()
            });

            #[cfg(feature = "work_steal")]
            scheduler.schedule_with_id(co, id);
            #[cfg(not(feature = "work_steal"))]
            crate::coroutine_impl::run_coroutine(co);
        }

        // run all the local tasks
        scheduler.run_queued_tasks(id);

        // free the unused event_data
        self.free_unused_event_data(id);

        // deal with the timer list
        #[cfg(feature = "io_timeout")]
        let next_expire = single_selector
            .timer_list
            .schedule_timer(now(), &timeout_handler);
        #[cfg(not(feature = "io_timeout"))]
        let next_expire = None;
        Ok(next_expire)
    }

    // this will post an os event so that we can wakeup the event loop
    #[inline]
    pub fn wakeup(&self, id: usize) {
        let selector = unsafe { self.vec.get_unchecked(id) };
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
    pub fn add_fd(&self, io_data: IoData) -> io::Result<IoData> {
        let fd = io_data.fd;
        let id = fd as usize % self.vec.len();
        let kqfd = unsafe { self.vec.get_unchecked(id) }.as_raw_fd();

        let flags = libc::EV_ADD | libc::EV_CLEAR | libc::EV_RECEIPT;
        let udata = io_data.as_ref() as *const _;
        let mut changes = [
            kevent!(fd, libc::EVFILT_READ, flags, udata),
            kevent!(fd, libc::EVFILT_WRITE, flags, udata),
        ];

        syscall!(kevent(
            kqfd,
            changes.as_ptr(),
            changes.len() as libc::c_int,
            changes.as_mut_ptr(),
            changes.len() as libc::c_int,
            ptr::null(),
        ))?;

        debug!("add fd to kqueue select, fd={:?}", fd);
        Ok(io_data)
    }

    #[inline]
    pub fn mod_fd(&self, io_data: &IoData, is_read: bool) -> io::Result<()> {
        let fd = io_data.fd;
        let id = fd as usize % self.vec.len();
        let kqfd = unsafe { self.vec.get_unchecked(id) }.as_raw_fd();

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
        #[cfg(feature = "io_timeout")]
        if let Some(h) = io_data.timer.borrow_mut().take() {
            unsafe {
                // mark the timer as removed if any, this only happened
                // when cancel an IO. what if the timer expired at the same time?
                // because we run this func in the user space, so the timer handler
                // will not got the coroutine
                h.with_mut_data(|value| value.data.event_data = ptr::null_mut());
            }
        }

        let fd = io_data.fd;
        let id = fd as usize % self.vec.len();
        let single_selector = unsafe { self.vec.get_unchecked(id) };
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
        // after EpollCtlDel push the unused event data
        single_selector.free_ev.push((*io_data).clone());
    }

    // we can't free the event data directly in the worker thread
    // must free them before the next epoll_wait
    #[inline]
    fn free_unused_event_data(&self, id: usize) {
        let free_ev = &unsafe { self.vec.get_unchecked(id) }.free_ev;
        while !free_ev.bulk_pop().is_empty() {}
    }

    // register the io request to the timeout list
    #[inline]
    #[cfg(feature = "io_timeout")]
    pub fn add_io_timer(&self, io: &IoData, timeout: Duration) {
        let id = io.fd as usize % self.vec.len();
        // info!("io timeout = {:?}", dur);
        let (h, b_new) = unsafe { self.vec.get_unchecked(id) }
            .timer_list
            .add_timer(timeout, io.timer_data());
        if b_new {
            // wakeup the event loop thread to recall the next wait timeout
            self.wakeup(id);
        }
        io.timer.borrow_mut().replace(h);
    }
}
