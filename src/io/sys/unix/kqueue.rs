use std::os::unix::io::RawFd;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use std::{io, ptr};

use super::{timeout_handler, EventData, IoData, TimerList};
use crate::scheduler::Scheduler;
use crate::timeout_list::{now, ns_to_dur};

use crossbeam::queue::SegQueue;
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

struct SingleSelector {
    kqfd: RawFd,
    timer_list: TimerList,
    free_ev: SegQueue<Arc<EventData>>,
}

impl SingleSelector {
    pub fn new() -> io::Result<Self> {
        let kqfd = unsafe { libc::kqueue() };
        if kqfd < 0 {
            return Err(io::Error::last_os_error());
        }

        let kev = libc::kevent {
            ident: NOTIFY_IDENT,
            filter: libc::EVFILT_USER,
            flags: libc::EV_ADD | libc::EV_CLEAR,
            fflags: 0,
            data: 0,
            udata: ptr::null_mut(),
        };

        let ret = unsafe { libc::kevent(kqfd, &kev, 1, ptr::null_mut(), 0, ptr::null()) };
        if ret < 0 {
            unsafe { libc::close(kqfd) };
            return Err(io::Error::last_os_error());
        }

        Ok(SingleSelector {
            kqfd,
            free_ev: SegQueue::new(),
            timer_list: TimerList::new(),
        })
    }
}

impl Drop for SingleSelector {
    fn drop(&mut self) {
        let _ = unsafe { libc::close(self.kqfd) };
    }
}

pub struct Selector {
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
        let timeout = timeout.map(|to| {
            let dur = ns_to_dur(to);
            libc::timespec {
                tv_sec: dur.as_secs() as libc::time_t,
                tv_nsec: dur.subsec_nanos() as libc::c_long,
            }
        });

        let timeout = timeout
            .as_ref()
            .map(|s| s as *const _)
            .unwrap_or(ptr::null_mut());
        // info!("select; timeout={:?}", timeout_ms);

        let single_selector = unsafe { self.vec.get_unchecked(id) };

        // Wait for kqueue events for at most timeout_ms milliseconds
        let kqfd = single_selector.kqfd;
        let n = unsafe {
            libc::kevent(
                kqfd,
                ptr::null(),
                0,
                events.as_mut_ptr(),
                events.len() as libc::c_int,
                timeout,
            )
        };

        if n < 0 {
            return Err(io::Error::last_os_error());
        }

        let n = n as usize;

        for event in unsafe { events.get_unchecked(..n) } {
            if event.udata.is_null() {
                // this is just a wakeup event, ignore it
                // let mut buf = [0u8; 8];
                // clear the eventfd, ignore the result
                // read(self.vec[id].evfd, &mut buf).ok();
                info!("got wakeup event in select, id={}", id);
                scheduler.collect_global(id);
                continue;
            }
            let data = unsafe { &mut *(event.udata as *mut EventData) };
            // info!("select got event, data={:p}", data);
            data.io_flag.store(true, Ordering::Release);

            // first check the atomic co, this may be grab by the worker first
            let co = match data.co.take(Ordering::Acquire) {
                None => continue,
                Some(co) => co,
            };

            // it's safe to remove the timer since we are running the timer_list in the same thread
            data.timer.borrow_mut().take().map(|h| {
                unsafe {
                    // tell the timer handler not to cancel the io
                    // it's not always true that you can really remove the timer entry
                    h.with_mut_data(|value| value.data.event_data = ptr::null_mut());
                }
                h.remove()
            });

            scheduler.schedule_with_id(co, id);
        }

        // run all the local tasks
        scheduler.run_queued_tasks(id);

        // free the unused event_data
        self.free_unused_event_data(id);

        // deal with the timer list
        let next_expire = single_selector
            .timer_list
            .schedule_timer(now(), &timeout_handler);
        Ok(next_expire)
    }

    // this will post an os event so that we can wakeup the event loop
    #[inline]
    pub fn wakeup(&self, id: usize) {
        let kqfd = unsafe { self.vec.get_unchecked(id) }.kqfd;
        let kev = libc::kevent {
            ident: NOTIFY_IDENT,
            filter: libc::EVFILT_USER,
            flags: 0,
            fflags: libc::NOTE_TRIGGER,
            data: 0,
            udata: ptr::null_mut(),
        };

        let ret = unsafe { libc::kevent(kqfd, &kev, 1, ptr::null_mut(), 0, ptr::null()) };

        trace!("wakeup id={:?}, ret={:?}", id, ret);
    }

    // register io event to the selector
    #[inline]
    pub fn add_fd(&self, io_data: IoData) -> io::Result<IoData> {
        let fd = io_data.fd;
        let id = fd as usize % self.vec.len();
        let kqfd = unsafe { self.vec.get_unchecked(id) }.kqfd;
        info!("add fd to kqueue select, fd={:?}", fd);

        let flags = libc::EV_ADD | libc::EV_CLEAR;
        let udata = io_data.as_ref() as *const _;
        let changes = [
            kevent!(fd, libc::EVFILT_READ, flags, udata),
            kevent!(fd, libc::EVFILT_WRITE, flags, udata),
        ];

        let n = unsafe {
            libc::kevent(
                kqfd,
                changes.as_ptr(),
                changes.len() as libc::c_int,
                ptr::null_mut(),
                0,
                ptr::null(),
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(io_data)
    }

    #[inline]
    pub fn mod_fd(&self, io_data: &IoData, is_read: bool) -> io::Result<()> {
        let fd = io_data.fd;
        let id = fd as usize % self.vec.len();
        let kqfd = unsafe { self.vec.get_unchecked(id) }.kqfd;
        info!("add fd to kqueue select, fd={:?}", fd);

        let flags = libc::EV_DELETE;
        let udata = io_data.as_ref() as *const _;
        let changes = if is_read {
            [kevent!(fd, libc::EVFILT_WRITE, flags, udata)]
        } else {
            [kevent!(fd, libc::EVFILT_READ, flags, udata)]
        };

        let n = unsafe {
            libc::kevent(
                kqfd,
                changes.as_ptr(),
                changes.len() as libc::c_int,
                ptr::null_mut(),
                0,
                ptr::null(),
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(())
    }

    #[inline]
    pub fn del_fd(&self, io_data: &IoData) {
        io_data.timer.borrow_mut().take().map(|h| {
            unsafe {
                // mark the timer as removed if any, this only happened
                // when cancel an IO. what if the timer expired at the same time?
                // because we run this func in the user space, so the timer handler
                // will not got the coroutine
                h.with_mut_data(|value| value.data.event_data = ptr::null_mut());
            }
        });

        let fd = io_data.fd;
        let id = fd as usize % self.vec.len();
        let single_selector = unsafe { self.vec.get_unchecked(id) };
        let kqfd = single_selector.kqfd;
        info!("del fd from kqueue select, fd={:?}", fd);

        let filter = libc::EV_DELETE;
        let changes = [
            kevent!(fd, libc::EVFILT_READ, filter, ptr::null_mut()),
            kevent!(fd, libc::EVFILT_WRITE, filter, ptr::null_mut()),
        ];
        // ignore the error
        unsafe {
            libc::kevent(
                kqfd,
                changes.as_ptr(),
                changes.len() as libc::c_int,
                ptr::null_mut(),
                0,
                ptr::null(),
            );
        }

        // after EpollCtlDel push the unused event data
        single_selector.free_ev.push((*io_data).clone());
    }

    // we can't free the event data directly in the worker thread
    // must free them before the next epoll_wait
    #[inline]
    fn free_unused_event_data(&self, id: usize) {
        let free_ev = &unsafe { self.vec.get_unchecked(id) }.free_ev;
        while free_ev.pop().is_some() {}
    }

    // register the io request to the timeout list
    #[inline]
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
