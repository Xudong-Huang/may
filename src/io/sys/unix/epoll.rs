use std::os::unix::io::RawFd;
use std::{io, cmp, ptr, isize};
use std::sync::atomic::Ordering;
use super::EventData;
use super::from_nix_error;
use super::nix::sys::epoll::*;
use super::nix::fcntl::FcntlArg::F_SETFL;
use super::nix::fcntl::{fcntl, O_CLOEXEC};
use super::nix::unistd::close;
use timeout_list::ns_to_ms;
use queue::mpsc_list::Queue as mpsc;

pub type SysEvent = EpollEvent;

pub struct Selector {
    epfd: RawFd,
    free_ev: mpsc<*mut EventData>,
}

impl Selector {
    pub fn new() -> io::Result<Selector> {
        let epfd = try!(epoll_create());
        try!(fcntl(epfd, F_SETFL(O_CLOEXEC)));
        Ok(Selector {
            epfd: epfd,
            free_ev: mpsc::new(),
        })
    }

    pub fn select(&self, events: &mut [SysEvent], timeout: Option<u64>) -> io::Result<()> {
        let timeout_ms = timeout.map(|to| cmp::min(ns_to_ms(to), isize::MAX as u64) as isize)
            .unwrap_or(-1);
        // info!("select; timeout={:?}", timeout_ms);
        // info!("polling epoll");

        // Wait for epoll events for at most timeout_ms milliseconds
        let n = try!(epoll_wait(self.epfd, events, timeout_ms).map_err(from_nix_error));

        for event in events[..n].iter() {
            if event.data == 0 {
                // this is just a wakeup event, ignore it
                // error!("got null data event in select");
                continue;
            }
            let data = unsafe { &mut *(event.data as *mut EventData) };
            // info!("select got event, data={:p}", data);
            data.io_flag.store(true, Ordering::Relaxed);

            // first check the atomic co, this may be grab by the worker first
            let co = data.co.take(Ordering::Acquire);
            if co.is_none() {
                // there is no coroutine prepared, just ignore this one
                // warn!("can't get coroutine in the epoll select");
                continue;
            }
            let mut co = co.unwrap();
            // co.prefetch();

            // it's safe to remove the timer since we are runing the timer_list in the same thread
            // data.timer.take().map(|h| {
            //     unsafe {
            //         // tell the timer function not to cancel the io
            //         // it's not always true that you can really remove the timer entry
            //         h.get_data().data.event_data = ptr::null_mut();
            //     }
            //     h.remove()
            // });

            // schedule the coroutine
            match co.resume() {
                Some(ev) => ev.subscribe(co),
                None => panic!("coroutine not return!"),
            }
        }

        // free the unused event_data
        self.free_unused_event_data();
        Ok(())
    }

    // this will post an os event so that we can wakeup the event loop
    #[inline]
    pub fn wakeup(&self) {
        // self.port.post(CompletionStatus::new(0, 0, ptr::null_mut())).unwrap();
    }

    // register io event to the selector
    #[inline]
    pub fn add_fd(&self, ev_data: &EventData) -> io::Result<()> {
        let info = EpollEvent {
            events: EPOLLIN | EPOLLOUT | EPOLLET,
            data: ev_data as *const _ as _,
        };
        let fd = ev_data.fd;
        info!("add fd to epoll select, fd={:?}", fd);
        epoll_ctl(self.epfd, EpollOp::EpollCtlAdd, fd, &info).map_err(from_nix_error)
    }

    #[inline]
    pub fn del_fd(&self, ev_data: &mut EventData) -> io::Result<()> {
        let info = EpollEvent {
            events: EpollEventKind::empty(),
            data: 0,
        };

        // can't del the fd from epoll, or next add_io would fail
        let fd = ev_data.fd;
        info!("remove io from epoll select, fd={:?}", fd);
        let ret = epoll_ctl(self.epfd, EpollOp::EpollCtlDel, fd, &info).map_err(from_nix_error);

        // after EpollCtlDel push the unused event data
        self.free_ev.push(ev_data);
        ret
    }

    // we can't free the event data directly in the worker thread
    // must free them before the next epoll_wait
    #[inline]
    fn free_unused_event_data(&self) {
        while let Some(ev) = self.free_ev.pop() {
            // let _ = unsafe { Box::from_raw(ev) };
        }
    }
}

impl Drop for Selector {
    fn drop(&mut self) {
        let _ = close(self.epfd);
    }
}
