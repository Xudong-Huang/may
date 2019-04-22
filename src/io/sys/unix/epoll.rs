use std::os::unix::io::RawFd;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use std::{cmp, io, isize, ptr};

use super::{from_nix_error, timeout_handler, EventData, IoData, TimerList};
use coroutine_impl::CoroutineImpl;
use libc::{eventfd, EFD_NONBLOCK};
use may_queue::mpsc_list::Queue as mpsc;
use nix::sys::epoll::*;
use nix::unistd::{close, read, write};
use smallvec::SmallVec;
use timeout_list::{now, ns_to_ms};

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
    timer_list: TimerList,
    free_ev: mpsc<Arc<EventData>>,
}

impl SingleSelector {
    pub fn new() -> io::Result<Self> {
        // wakeup data is 0
        let mut info = EpollEvent::new(EpollFlags::EPOLLET | EpollFlags::EPOLLIN, 0);

        let epfd = epoll_create().map_err(from_nix_error)?;
        let evfd = create_eventfd()?;

        // add the eventfd to the epfd
        epoll_ctl(epfd, EpollOp::EpollCtlAdd, evfd, &mut info).map_err(from_nix_error)?;

        Ok(SingleSelector {
            epfd,
            evfd,
            free_ev: mpsc::new(),
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
    schedule_policy: fn(CoroutineImpl),
}

impl Selector {
    pub fn new(io_workers: usize, schedule_policy: fn(CoroutineImpl)) -> io::Result<Self> {
        let mut s = Selector {
            schedule_policy,
            vec: SmallVec::new(),
        };

        for _ in 0..io_workers {
            let ss = SingleSelector::new()?;
            s.vec.push(ss);
        }

        Ok(s)
    }

    pub fn select(
        &self,
        id: usize,
        events: &mut [SysEvent],
        timeout: Option<u64>,
    ) -> io::Result<Option<u64>> {
        let mut ev = EpollEvent::new(EpollFlags::EPOLLIN, 0);
        let timeout_ms = timeout
            .map(|to| cmp::min(ns_to_ms(to), isize::MAX as u64) as isize)
            .unwrap_or(-1);
        // info!("select; timeout={:?}", timeout_ms);

        // Wait for epoll events for at most timeout_ms milliseconds
        let epfd = self.vec[id].epfd;
        let evfd = self.vec[id].evfd;
        let n = epoll_wait(epfd, events, timeout_ms).map_err(from_nix_error)?;

        // add this would increase the performance!!!!!!!!
        // this maybe a linux bug, the code is meaningless
        epoll_ctl(epfd, EpollOp::EpollCtlDel, evfd, &mut ev).ok();
        epoll_ctl(epfd, EpollOp::EpollCtlAdd, evfd, &mut ev).ok();

        for event in events[..n].iter() {
            if event.data() == 0 {
                // this is just a wakeup event, ignore it
                let mut buf = [0u8; 8];
                // clear the eventfd, ignore the result
                read(self.vec[id].evfd, &mut buf).ok();
                // info!("got wakeup event in select, id={}", id);
                continue;
            }
            let data = unsafe { &mut *(event.data() as *mut EventData) };
            // info!("select got event, data={:p}", data);
            data.io_flag.store(true, Ordering::Release);

            // first check the atomic co, this may be grab by the worker first
            let co = match data.co.take(Ordering::Acquire) {
                None => continue,
                Some(co) => co,
            };
            co.prefetch();

            // it's safe to remove the timer since we are running the timer_list in the same thread
            data.timer.borrow_mut().take().map(|h| {
                unsafe {
                    // tell the timer handler not to cancel the io
                    // it's not always true that you can really remove the timer entry
                    h.with_mut_data(|value| value.data.event_data = ptr::null_mut());
                }
                h.remove()
            });

            // schedule the coroutine
            (self.schedule_policy)(co);
        }

        // free the unused event_data
        self.free_unused_event_data(id);

        // deal with the timer list
        let next_expire = self.vec[id]
            .timer_list
            .schedule_timer(now(), &timeout_handler);
        Ok(next_expire)
    }

    // this will post an os event so that we can wake up the event loop
    #[inline]
    fn wakeup(&self, id: usize) {
        let buf = unsafe { ::std::slice::from_raw_parts(&1u64 as *const u64 as _, 8) };
        let ret = write(self.vec[id].evfd, buf);
        info!("wakeup id={:?}, ret={:?}", id, ret);
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
        let epfd = self.vec[id].epfd;
        info!("add fd to epoll select, fd={:?}", fd);
        epoll_ctl(epfd, EpollOp::EpollCtlAdd, fd, &mut info)
            .map_err(from_nix_error)
            .map(|_| io_data)
    }

    #[inline]
    pub fn del_fd(&self, io_data: &IoData) {
        use std::ops::Deref;

        let mut info = EpollEvent::empty();

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
        let epfd = self.vec[id].epfd;
        info!("del fd from epoll select, fd={:?}", fd);
        epoll_ctl(epfd, EpollOp::EpollCtlDel, fd, &mut info).ok();

        // after EpollCtlDel push the unused event data
        self.vec[id].free_ev.push(io_data.deref().clone());
    }

    // we can't free the event data directly in the worker thread
    // must free them before the next epoll_wait
    #[inline]
    fn free_unused_event_data(&self, id: usize) {
        let free_ev = &self.vec[id].free_ev;
        while let Some(_) = free_ev.pop() {}
    }

    // register the io request to the timeout list
    #[inline]
    pub fn add_io_timer(&self, io: &IoData, timeout: Option<Duration>) {
        let id = io.fd as usize % self.vec.len();
        *io.timer.borrow_mut() = timeout.map(|dur| {
            // info!("io timeout = {:?}", dur);
            let (h, b_new) = self.vec[id].timer_list.add_timer(dur, io.timer_data());
            if b_new {
                // wake up the event loop thread to recall the next wait timeout
                self.wakeup(id);
            }
            h
        });
    }
}
