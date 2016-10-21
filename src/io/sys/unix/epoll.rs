use std::time::Duration;
use std::os::unix::io::RawFd;
use std::{io, cmp, ptr, isize};
use timeout_list::{now, ns_to_ms};
use super::nix::sys::epoll::*;
use super::nix::fcntl::FcntlArg::F_SETFL;
use super::nix::fcntl::{fcntl, O_CLOEXEC};
use super::nix::unistd::close;
use super::{EventFlags, FLAG_READ, FLAG_WRITE};
use super::{EventData, TimerList, from_nix_error, timeout_handler};

// covert interested event into system EpollEventKind
#[inline]
fn interest_to_epoll_kind(interest: EventFlags) -> EpollEventKind {
    let mut kind = EpollEventKind::from(EPOLLONESHOT | EPOLLET);

    if interest.contains(FLAG_READ) {
        kind.insert(EPOLLIN);
    }

    if interest.contains(FLAG_WRITE) {
        kind.insert(EPOLLOUT);
    }
    // kind.insert(EPOLLRDHUP);
    kind
}

pub type SysEvent = EpollEvent;


pub struct Selector {
    epfd: Vec<RawFd>,
    timer_list: Vec<TimerList>,
}

impl Selector {
    pub fn new(io_workers: usize) -> io::Result<Selector> {
        let mut epfd = vec![0; io_workers];
        for i in 0..io_workers {
            let fd = try!(epoll_create().map_err(from_nix_error));
            try!(fcntl(fd, F_SETFL(O_CLOEXEC)).map_err(from_nix_error));
            epfd[i] = fd;
        }

        let timer_list = (0..io_workers).map(|_| TimerList::new()).collect();

        Ok(Selector {
            epfd: epfd,
            timer_list: timer_list,
        })
    }

    pub fn select(&self,
                  id: usize,
                  events: &mut [SysEvent],
                  timeout: Option<u64>)
                  -> io::Result<Option<u64>> {
        let timeout_ms = timeout.map(|to| cmp::min(ns_to_ms(to), isize::MAX as u64) as isize)
            .unwrap_or(-1);
        // info!("select; timeout={:?}", timeout_ms);
        // info!("polling epoll");

        // Wait for epoll events for at most timeout_ms milliseconds
        let n = try!(epoll_wait(self.epfd[id], events, timeout_ms).map_err(from_nix_error));

        for event in events[..n].iter() {
            if event.data == 0 {
                // this is just a wakeup event, ignore it
                // error!("got null data event in select");
                continue;
            }
            let data = unsafe { &mut *(event.data as *mut EventData) };
            // info!("select got event, data={:p}", data);

            let mut co = data.co.take().expect("can't get co in selector");
            co.prefetch();

            // it's safe to remove the timer since we are runing the timer_list in the same thread
            data.timer.take().map(|h| {
                unsafe {
                    // tell the timer hanler not to cancel the io
                    // it's not always true that you can really remove the timer entry
                    h.get_data().data.event_data = ptr::null_mut();
                }
                h.remove()
            });

            // schedule the coroutine
            match co.resume() {
                Some(ev) => ev.subscribe(co),
                None => panic!("coroutine not return!"),
            }
        }

        // deal with the timer list
        let next_expire = self.timer_list[id].schedule_timer(now(), &timeout_handler);
        Ok(next_expire)
    }

    // this will post an os event so that we can wakeup the event loop
    #[inline]
    fn wakeup(&self, _id: usize) {
        // self.port.post(CompletionStatus::new(0, 0, ptr::null_mut())).unwrap();
    }

    // register io event to the selector
    // #[inline]
    // pub fn add_fd(&self, fd: RawFd) -> io::Result<()> {
    //     let info = EpollEvent {
    //         events: EpollEventKind::empty(),
    //         data: 0,
    //     };
    //     let epfd = self.epfd[fd as usize % self.epfd.len()];
    //     info!("add fd to epoll select, fd={:?}", fd);
    //     epoll_ctl(epfd, EpollOp::EpollCtlAdd, fd, &info).map_err(from_nix_error)
    // }

    // register io event to the selector
    #[inline]
    pub fn add_io(&self, ev_data: &EventData) -> io::Result<()> {
        let info = EpollEvent {
            events: interest_to_epoll_kind(ev_data.interest),
            data: ev_data as *const _ as _,
        };
        let fd = ev_data.fd;
        let epfd = self.epfd[fd as usize % self.epfd.len()];
        info!("mod fd to epoll select, fd={:?}", fd);
        epoll_ctl(epfd, EpollOp::EpollCtlAdd, fd, &info).map_err(from_nix_error)
    }

    #[inline]
    pub fn del_fd(&self, fd: RawFd) {
        let info = EpollEvent {
            events: EpollEventKind::empty(),
            data: 0,
        };
        let epfd = self.epfd[fd as usize % self.epfd.len()];
        info!("add fd to epoll select, fd={:?}", fd);
        epoll_ctl(epfd, EpollOp::EpollCtlDel, fd, &info).ok();
    }

    // register the io request to the timeout list
    #[inline]
    pub fn add_io_timer(&self, io: &mut EventData, timeout: Option<Duration>) {
        let id = io.fd as usize % self.epfd.len();
        io.timer = timeout.map(|dur| {
            // info!("io timeout = {:?}", dur);
            let (h, b_new) = self.timer_list[id].add_timer(dur, io.timer_data());
            if b_new {
                // wakeup the event loop threead to recal the next wait timeout
                self.wakeup(id);
            }
            h
        });
    }
}

impl Drop for Selector {
    fn drop(&mut self) {
        for fd in &self.epfd {
            let _ = close(*fd);
        }
    }
}
