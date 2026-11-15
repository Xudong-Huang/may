use std::io;
use std::os::fd::AsFd;
use std::os::fd::BorrowedFd;
#[cfg(feature = "io_timeout")]
use std::time::Duration;

use super::select_loop::{Ready, ShardState};
use super::{from_nix_error, EventData, IoData};
use crate::scheduler::Scheduler;
#[cfg(feature = "io_timeout")]
use crate::sync::WaitCo;

use nix::sys::epoll::*;
use nix::sys::eventfd::*;
use smallvec::SmallVec;

pub type SysEvent = EpollEvent;

struct SingleSelector {
    epoll: Epoll,
    evfd: EventFd,
    state: ShardState,
}

impl SingleSelector {
    pub fn new() -> io::Result<Self> {
        // wakeup data is 0
        let info = EpollEvent::new(EpollFlags::EPOLLIN, 0);

        let epoll = Epoll::new(EpollCreateFlags::EPOLL_CLOEXEC)?;
        let evfd = EventFd::from_flags(EfdFlags::EFD_NONBLOCK | EfdFlags::EFD_CLOEXEC)?;

        // add the eventfd to the epfd
        epoll.add(evfd.as_fd(), info)?;

        Ok(SingleSelector {
            epoll,
            evfd,
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
        let epoll = &single_selector.epoll;
        let evfd = &single_selector.evfd;

        single_selector.state.select(
            scheduler,
            id,
            events,
            timeout,
            |remaining, events| {
                let timeout = remaining
                    .map(|to| {
                        let ms = to.div_ceil(1_000_000);
                        // Clamp to i32::MAX milliseconds if the timeout exceeds that.
                        // EpollTimeout::try_from rejects values > i32::MAX, so we cap it.
                        let ms_clamped = ms.min(i32::MAX as u64) as i32;
                        EpollTimeout::try_from(ms_clamped).unwrap()
                    })
                    .unwrap_or(EpollTimeout::NONE);
                // info!("select; timeout={:?}", timeout_ms);

                // Wait for epoll events for at most timeout_ms milliseconds
                epoll.wait(events, timeout).map_err(from_nix_error)
            },
            |event| {
                if event.data() == 0 {
                    // this is just a wakeup event, ignore it
                    // clear the eventfd, ignore the result
                    evfd.read().ok();
                    // info!("got wakeup event in select, id={}", id);
                    Ready::Wakeup
                } else {
                    // the event data is shared through `Arc`, so only a shared
                    // reference may be created here; everything touched on it is
                    // interior-mutable (atomics / `AtomicOption`)
                    let data = unsafe { &*(event.data() as *const EventData) };
                    // info!("select got event, data={:p}", data);
                    Ready::Io(data, event.events().bits() as usize)
                }
            },
        )
    }

    // this will post an os event so that we can wake up the event loop
    #[inline]
    pub fn wakeup(&self, id: usize) {
        let ret = self.vec[id].evfd.write(1);
        trace!("wakeup id={id:?}, ret={ret:?}");
    }

    // register io event to the selector
    #[inline]
    pub fn add_fd(&self, mut io_data: IoData) -> io::Result<IoData> {
        let info = EpollEvent::new(
            EpollFlags::EPOLLIN
                | EpollFlags::EPOLLOUT
                | EpollFlags::EPOLLRDHUP
                | EpollFlags::EPOLLET,
            io_data.as_ref() as *const _ as _,
        );

        let fd = io_data.fd;
        let id = self.shard_id(fd);
        let single_selector = &self.vec[id];
        let epoll = &single_selector.epoll;
        info!("add fd to epoll select, fd={fd:?}");
        // Only mark the io data as registered after a successful ADD. If this
        // fails with EEXIST (the fd is already registered, e.g. a second CoIo
        // over the same fd), dropping the io data must not issue a DEL: it
        // would remove the existing registration and the original owner would
        // silently stop receiving io events.
        epoll
            .add(unsafe { BorrowedFd::borrow_raw(fd) }, info)
            .map_err(from_nix_error)?;
        io_data.mark_registered();
        Ok(io_data)
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
                EpollFlags::EPOLLOUT | EpollFlags::EPOLLHUP | EpollFlags::EPOLLET,
                io_data.as_ref() as *const _ as _,
            )
        };

        let fd = io_data.fd;
        let id = self.shard_id(fd);
        let single_selector = &self.vec[id];
        let epoll = &single_selector.epoll;
        info!("mod fd to epoll select, fd={fd:?}, is_read={is_read}");
        epoll
            .modify(unsafe { BorrowedFd::borrow_raw(fd) }, &mut info)
            .map_err(from_nix_error)
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
        let epoll = &single_selector.epoll;
        info!("del fd from epoll select, fd={fd:?}");
        epoll.delete(unsafe { BorrowedFd::borrow_raw(fd) }).ok();

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
            // wake up the event loop thread to recall the next wait timeout
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    /// A deregistration from another thread must wake the selector that owns
    /// the fd's shard. `free_ev` is drained only by that thread, at the end of
    /// its `select`; `epoll_ctl(DEL)` raises no event, and an idle selector
    /// blocks in `epoll_wait` with no timeout when the `io_timeout` poll is
    /// off. Without the wakeup every closed fd's `Arc<EventData>` would be
    /// retained until some unrelated event arrived.
    #[test]
    fn del_fd_wakes_the_owning_selector() {
        let selector = Selector::new(1).unwrap();
        let socket = std::net::UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        // never registered, so its `Drop` does not touch the global scheduler
        let io_data = IoData::new(&socket);

        std::thread::scope(|s| {
            s.spawn(|| selector.del_fd(&io_data));
        });

        // the eventfd is the only registration in this selector, so a
        // delivered event can only be the free-queue wakeup (data 0)
        let mut events = [EpollEvent::new(EpollFlags::EPOLLIN, 0); 4];
        let n = selector.vec[0]
            .epoll
            .wait(&mut events, EpollTimeout::ZERO)
            .unwrap();
        assert_eq!(n, 1, "del_fd did not wake the owning selector");
        assert_eq!(events[0].data(), 0, "unexpected non-wakeup event");

        assert_eq!(selector.vec[0].state.free_ev.len(), 1);
        selector.vec[0].state.drain_free_ev();
        assert!(selector.vec[0].state.free_ev.is_empty());
        assert!(!selector.vec[0].state.free_ev_armed.load(Ordering::Acquire));
    }
}
