use std::os::unix::io::RawFd;
use std::sync::atomic::Ordering;
use std::sync::Arc;
#[cfg(feature = "io_timeout")]
use std::time::Duration;
use std::{cmp, io, isize};

use super::{from_nix_error, EventData, IoData};
#[cfg(feature = "io_timeout")]
use super::{timeout_handler, TimerList};
use crate::coroutine_impl::{run_coroutine, CoroutineImpl};
use crate::scheduler::Scheduler;
#[cfg(feature = "io_timeout")]
use crate::timeout_list::now;
use crate::timeout_list::ns_to_ms;

use crossbeam::queue::SegQueue;
use libc::{eventfd, EFD_NONBLOCK};
use nix::sys::epoll::*;
use nix::unistd::{close, read, write};
use smallvec::SmallVec;

fn create_eventfd() -> io::Result<RawFd> {
    let fd = unsafe { eventfd(0, EFD_NONBLOCK) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd as RawFd)
}

pub type SysEvent = EpollEvent;

struct SingleSelector {
    epfd: RawFd,
    evfd: RawFd,
    #[cfg(feature = "io_timeout")]
    timer_list: TimerList,
    free_ev: SegQueue<Arc<EventData>>,
}

impl SingleSelector {
    pub fn new() -> io::Result<Self> {
        // wakeup data is 0
        let mut info = EpollEvent::new(EpollFlags::EPOLLET | EpollFlags::EPOLLIN, 0);

        let epfd = epoll_create1(EpollCreateFlags::EPOLL_CLOEXEC).map_err(from_nix_error)?;
        let evfd = match create_eventfd() {
            Ok(fd) => fd,
            Err(err) => {
                let _ = close(epfd);
                return Err(err);
            }
        };

        // add the eventfd to the epfd
        if let Err(e) = epoll_ctl(epfd, EpollOp::EpollCtlAdd, evfd, &mut info) {
            let _ = close(evfd);
            let _ = close(epfd);
            return Err(from_nix_error(e));
        };

        Ok(SingleSelector {
            epfd,
            evfd,
            free_ev: SegQueue::new(),
            #[cfg(feature = "io_timeout")]
            timer_list: TimerList::new(),
        })
    }
}

impl Drop for SingleSelector {
    fn drop(&mut self) {
        let _ = close(self.evfd);
        let _ = close(self.epfd);
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
        let timeout_ms = timeout
            .map(|to| cmp::min(ns_to_ms(to), isize::MAX as u64) as isize)
            .unwrap_or(-1);
        // info!("select; timeout={:?}", timeout_ms);

        let single_selector = unsafe { self.vec.get_unchecked(id) };
        let epfd = single_selector.epfd;

        let mut co_vec: SmallVec<[CoroutineImpl; 2]> = SmallVec::new();

        // Wait for epoll events for at most timeout_ms milliseconds
        let n = epoll_wait(epfd, events, timeout_ms).map_err(from_nix_error)?;

        // collect coroutines
        for event in events[..n].iter() {
            if event.data() == 0 {
                // this is just a wakeup event, ignore it
                let mut buf = [0u8; 8];
                // clear the eventfd, ignore the result
                while read(single_selector.evfd, &mut buf).is_ok() {}
                // info!("got wakeup event in select, id={}", id);
                continue;
            }
            let data = unsafe { &mut *(event.data() as *mut EventData) };
            // info!("select got event, data={:p}", data);
            data.io_flag.store(true, Ordering::Release);

            // first check the atomic co, this may be grab by the worker first
            let co = match data.co.take(Ordering::Acquire) {
                Some(co) => co,
                None => continue,
            };

            // it's safe to remove the timer since we are running the timer_list in the same thread
            #[cfg(feature = "io_timeout")]
            data.timer.borrow_mut().take().map(|h| {
                unsafe {
                    // tell the timer handler not to cancel the io
                    // it's not always true that you can really remove the timer entry
                    h.with_mut_data(|value| value.data.event_data = std::ptr::null_mut());
                }
                h.remove()
            });

            if co_vec.len() < co_vec.capacity() {
                co_vec.push(co);
            } else {
                scheduler.schedule_with_id(co, id);
            }
        }

        // schedule the io coroutine
        while let Some(co) = co_vec.pop() {
            if let Some(next) = co_vec.last() {
                next.prefetch();
            }
            run_coroutine(co);
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

    // this will post an os event so that we can wake up the event loop
    #[inline]
    pub fn wakeup(&self, id: usize) {
        let buf = 1u64.to_le_bytes();
        let ret = write(unsafe { self.vec.get_unchecked(id) }.evfd, &buf);
        trace!("wakeup id={:?}, ret={:?}", id, ret);
    }

    // register io event to the selector
    #[inline]
    pub fn add_fd(&self, io_data: IoData) -> io::Result<IoData> {
        let mut info = EpollEvent::new(
            EpollFlags::EPOLLIN
                | EpollFlags::EPOLLOUT
                | EpollFlags::EPOLLRDHUP
                | EpollFlags::EPOLLET,
            io_data.as_ref() as *const _ as _,
        );

        let fd = io_data.fd;
        let id = fd as usize % self.vec.len();
        let single_selector = unsafe { self.vec.get_unchecked(id) };
        let epfd = single_selector.epfd;
        info!("add fd to epoll select, fd={:?}", fd);
        epoll_ctl(epfd, EpollOp::EpollCtlAdd, fd, &mut info)
            .map_err(from_nix_error)
            .map(|_| io_data)
    }

    #[inline]
    pub fn mod_fd(&self, io_data: &IoData, is_read: bool) -> io::Result<()> {
        let mut info = if is_read {
            EpollEvent::new(
                EpollFlags::EPOLLIN | EpollFlags::EPOLLRDHUP | EpollFlags::EPOLLET,
                io_data.as_ref() as *const _ as _,
            )
        } else {
            EpollEvent::new(
                EpollFlags::EPOLLOUT | EpollFlags::EPOLLRDHUP | EpollFlags::EPOLLET,
                io_data.as_ref() as *const _ as _,
            )
        };

        let fd = io_data.fd;
        let id = fd as usize % self.vec.len();
        let single_selector = unsafe { self.vec.get_unchecked(id) };
        let epfd = single_selector.epfd;
        info!("mod fd to epoll select, fd={:?}, is_read={}", fd, is_read);
        epoll_ctl(epfd, EpollOp::EpollCtlMod, fd, &mut info).map_err(from_nix_error)
    }

    #[inline]
    pub fn del_fd(&self, io_data: &IoData) {
        let mut info = EpollEvent::empty();

        #[cfg(feature = "io_timeout")]
        if let Some(h) = io_data.timer.borrow_mut().take() {
            unsafe {
                // mark the timer as removed if any, this only happened
                // when cancel an IO. what if the timer expired at the same time?
                // because we run this func in the user space, so the timer handler
                // will not got the coroutine
                h.with_mut_data(|value| value.data.event_data = std::ptr::null_mut());
            }
        }

        let fd = io_data.fd;
        let id = fd as usize % self.vec.len();
        let single_selector = unsafe { self.vec.get_unchecked(id) };
        let epfd = single_selector.epfd;
        info!("del fd from epoll select, fd={:?}", fd);
        epoll_ctl(epfd, EpollOp::EpollCtlDel, fd, &mut info).ok();

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
    #[cfg(feature = "io_timeout")]
    pub fn add_io_timer(&self, io: &IoData, timeout: Duration) {
        let id = io.fd as usize % self.vec.len();
        // info!("io timeout = {:?}", dur);
        let (h, b_new) = unsafe { self.vec.get_unchecked(id) }
            .timer_list
            .add_timer(timeout, io.timer_data());
        if b_new {
            // wake up the event loop thread to recall the next wait timeout
            self.wakeup(id);
        }
        io.timer.borrow_mut().replace(h);
    }
}
