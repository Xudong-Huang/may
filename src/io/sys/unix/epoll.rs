use std::{cmp, io, i32};
use std::os::unix::io::RawFd;
use super::nix::sys::epoll::*;
use super::nix::unistd::close;
use super::{EventFlags, FLAG_READ, FLAG_WRITE, EventData};
use smallvec::SmallVec;
use scheduler::Scheduler;
use coroutine::CoroutineImpl;
use yield_now::set_co_para;
use timeout_list::{TimeoutHandle, ns_to_ms};

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
    // kind.insert(EPOLLET);
    kind
}

// buffer to receive the system events
pub type EventsBuf = SmallVec<[EpollEvent; 1024]>;

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
                  events: &mut EventsBuf,
                  timeout: Option<u64>)
                  -> io::Result<()> {

        let timeout_ms = timeout.map(|to| cmp::min(ns_to_ms(to), i32::MAX as u64) as i32)
            .unwrap_or(-1);
        info!("select; timeout={:?}", timeout_ms);
        info!("polling epoll");

        // Wait for epoll events for at most timeout_ms milliseconds
        let cnt = try!(epoll_wait(self.epfd, events, timeout_ms as isize)
            .map_err(super::from_nix_error));

        // unsafe {
        //     self.evts.events.set_len(cnt);
        // }
        //
        // for i in 0..cnt {
        //     let value = self.evts.events[i];
        //     let mut ev_flag = EventFlags::empty();
        //     if value.events.contains(EPOLLIN) {
        //         ev_flag = ev_flag | FLAG_READ;
        //     }
        //     if value.events.contains(EPOLLOUT) {
        //         ev_flag = ev_flag | FLAG_WRITE;
        //     }
        //     evts.push(EventEntry::new_evfd(value.data as u32, ev_flag));
        // }
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

        epoll_ctl(self.epfd, EpollOp::EpollCtlAdd, io.fd, &info).map_err(super::from_nix_error)
    }

    #[inline]
    pub fn cancel_io(&self, io: &EventData) -> io::Result<()> {
        let info = EpollEvent {
            events: interest_to_epoll_kind(io.interest),
            data: 0,
        };

        epoll_ctl(self.epfd, EpollOp::EpollCtlDel, io.fd, &info).map_err(super::from_nix_error)
    }
}

impl Drop for Selector {
    fn drop(&mut self) {
        let _ = close(self.epfd);
    }
}
