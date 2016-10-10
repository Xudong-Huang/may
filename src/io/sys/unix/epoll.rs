use std::os::unix::io::RawFd;
use std::{io, cmp, ptr, isize};
use super::from_nix_error;
use super::nix::sys::epoll::*;
use super::nix::unistd::close;
use super::{EventFlags, FLAG_READ, FLAG_WRITE, EventData};
use scheduler::Scheduler;
use timeout_list::ns_to_ms;

// covert interested event into system EpollEventKind
#[inline]
fn interest_to_epoll_kind(interest: EventFlags) -> EpollEventKind {
    let mut kind = EpollEventKind::empty();

    if interest.contains(FLAG_READ) {
        kind.insert(EPOLLIN);
    }

    if interest.contains(FLAG_WRITE) {
        kind.insert(EPOLLOUT);
    }

    kind.insert(EPOLLONESHOT);
    kind.insert(EPOLLET);
    kind
}

pub type SysEvent = EpollEvent;

pub struct Selector {
    epfd: RawFd,
}

impl Selector {
    pub fn new() -> io::Result<Selector> {
        let epfd = try!(epoll_create());
        Ok(Selector { epfd: epfd })
    }

    pub fn select(&self,
                  s: &Scheduler,
                  events: &mut [SysEvent],
                  timeout: Option<u64>)
                  -> io::Result<()> {

        let timeout_ms = timeout.map(|to| cmp::min(ns_to_ms(to), isize::MAX as u64) as isize)
            .unwrap_or(-1);
        info!("select; timeout={:?}", timeout_ms);
        info!("polling epoll");

        // Wait for epoll events for at most timeout_ms milliseconds
        let n = try!(epoll_wait(self.epfd, events, timeout_ms).map_err(from_nix_error));

        for event in events[..n].iter() {
            if event.data == 0 {
                // this is just a wakeup event, ignore it
                warn!("got null data event in select");
                continue;
            }
            let data = unsafe { &mut *(event.data as *mut EventData) };
            info!("select got event, event.events = {:?}", event.events);

            // it's safe to remove the timer since we are runing the timer_list in the same thread
            data.timer.take().map(|h| {
                unsafe {
                    // tell the timer function not to cancel the io
                    // it's not always true that you can really remove the timer entry
                    h.get_data().data.event_data = ptr::null_mut();
                }
                h.remove()
            });

            let co = data.co.take();
            if co.is_none() {
                // there is no coroutine prepared, just ignore this one
                warn!("can't get coroutine in the epoll select");
                continue;
            }
            let co = co.unwrap();

            // schedule the coroutine
            s.schedule_io(co);
        }

        Ok(())
    }

    // this will post an os event so that we can wakeup the event loop
    #[inline]
    pub fn wakeup(&self) {
        // self.port.post(CompletionStatus::new(0, 0, ptr::null_mut())).unwrap();
    }

    // register io event to the selector
    #[inline]
    pub fn add_io(&self, io: &mut EventData) -> io::Result<()> {
        let info = EpollEvent {
            events: interest_to_epoll_kind(io.interest),
            data: io as *mut _ as u64,
        };
        info!("add io to epoll select, fd={:?} events={:?}",
              io.fd,
              info.events);
        epoll_ctl(self.epfd, EpollOp::EpollCtlMod, io.fd, &info).map_err(from_nix_error)
    }

    // register io event to the selector
    #[inline]
    pub fn add_fd(&self, fd: RawFd) -> io::Result<()> {
        let info = EpollEvent {
            events: interest_to_epoll_kind(EventFlags::empty()),
            data: 0,
        };
        info!("add fd to epoll select, fd={:?}", fd);
        epoll_ctl(self.epfd, EpollOp::EpollCtlAdd, fd, &info).map_err(from_nix_error)
    }

    #[inline]
    pub fn cancel_io(&self, io: &EventData) -> io::Result<()> {
        let info = EpollEvent {
            events: interest_to_epoll_kind(EventFlags::empty()),
            data: 0,
        };

        // can't del the fd from epoll, or next add_io would fail
        println!("remove io from epoll select, fd={:?}", io.fd);
        epoll_ctl(self.epfd, EpollOp::EpollCtlMod, io.fd, &info).map_err(from_nix_error)
    }
}

impl Drop for Selector {
    fn drop(&mut self) {
        let _ = close(self.epfd);
    }
}
