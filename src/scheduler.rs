use std::io;
use std::thread;
use std::cell::Cell;
use std::time::Duration;
use std::intrinsics::likely;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Once, ONCE_INIT};
use smallvec::SmallVec;
use queue::BLOCK_SIZE;
use queue::mpmc::Queue as mpmc;
use queue::mpmc_v1::Queue as generic_mpmc;
use queue::mpmc_bounded::Queue as WaitList;
use queue::stack_ringbuf::RingBuf;
use timeout_list;
use sync::AtomicOption;
use pool::CoroutinePool;
use yield_now::set_co_para;
use config::scheduler_config;
use io::{EventLoop, Selector};
use coroutine::{CoroutineImpl, run_coroutine};

const ID_INIT: usize = ::std::usize::MAX;
thread_local!{static ID: Cell<usize> = Cell::new(ID_INIT);}

const PREFTCH_SIZE: usize = 4;

// here we use Arc<AtomicOption<>> for that in the select implementaion
// other event may try to consume the coroutine while timer thread consume it
type TimerData = Arc<AtomicOption<CoroutineImpl>>;
type TimerThread = timeout_list::TimerThread<TimerData>;

// filter out the cancel panic, don't print anything for it
fn filter_cancel_panic() {
    use ::std::panic;
    use generator::Error;
    let old = panic::take_hook();
    ::std::panic::set_hook(Box::new(move |info| {
        match info.payload().downcast_ref::<Error>() {
            // this is not an error at all, ignore it
            Some(_e @ &Error::Cancel) => return,
            _ => {}
        }
        old(info);
    }));
}

#[inline]
pub fn get_scheduler() -> &'static Scheduler {
    static mut SCHED: *const Scheduler = 0 as *const _;
    unsafe {
        if likely(!SCHED.is_null()) {
            return &*SCHED;
        }
    }

    static ONCE: Once = ONCE_INIT;
    ONCE.call_once(|| {
        let workers = scheduler_config().get_workers();
        let io_workers = scheduler_config().get_io_workers();
        let b: Box<Scheduler> = Scheduler::new(workers, io_workers);
        unsafe {
            SCHED = Box::into_raw(b);
        }

        // run the workers in background
        for id in 0..workers {
            thread::spawn(move || {
                filter_cancel_panic();
                ID.with(|m_id| m_id.set(id));
                let s = unsafe { &*SCHED };
                s.run(id);
            });
        }

        // timer thread
        thread::spawn(move || {
            filter_cancel_panic();
            let s = unsafe { &*SCHED };
            // timer function
            let timer_event_handler = |co: Arc<AtomicOption<CoroutineImpl>>| {
                // just repush the co to the visit list
                co.take_fast(Ordering::Relaxed).map(|mut c| {
                    // set the timeout result for the coroutine
                    set_co_para(&mut c, io::Error::new(io::ErrorKind::TimedOut, "timeout"));
                    s.schedule(c);
                });
            };

            s.timer_thread.run(&timer_event_handler);
        });

        // io event loop thread
        for id in 0..io_workers {
            thread::spawn(move || {
                filter_cancel_panic();
                let s = unsafe { &*SCHED };
                s.event_loop.run(id).unwrap_or_else(|e| {
                    panic!("event_loop failed running, err={}", e);
                });
            });
        }
    });
    unsafe { &*SCHED }
}

pub struct Scheduler {
    pub pool: CoroutinePool,
    event_loop: EventLoop,
    ready_list: mpmc<CoroutineImpl>,
    visit_list: generic_mpmc<CoroutineImpl>,
    wait_list: WaitList<thread::Thread>,
    timer_thread: TimerThread,
}

type StackVec = SmallVec<[CoroutineImpl; BLOCK_SIZE]>;
type CacheBuf = RingBuf<[CoroutineImpl; PREFTCH_SIZE + 2]>;

impl Scheduler {
    pub fn new(workers: usize, io_workers: usize) -> Box<Self> {
        Box::new(Scheduler {
            pool: CoroutinePool::new(),
            event_loop: EventLoop::new(io_workers).expect("can't create event_loop"),
            ready_list: mpmc::new(workers),
            timer_thread: TimerThread::new(),
            visit_list: generic_mpmc::new(workers),
            wait_list: WaitList::with_capacity(256), // workers: workers,
        })
    }

    #[inline]
    fn run_coroutines(&self, vec: &mut StackVec, old: &mut CacheBuf, size: usize) {
        let mut drain = vec.drain();
        // prefech one coroutine
        let mut j = 0;
        while j < PREFTCH_SIZE && j < size {
            let co = drain.next().expect("failed to drain co from vec");
            co.prefetch();
            unsafe {
                old.push_unchecked(co);
            }
            j += 1;
        }

        j = 0;
        while (j as isize) < (size as isize) - (PREFTCH_SIZE as isize) {
            let co = drain.next().expect("failed to drain co from vec");
            co.prefetch();
            unsafe {
                old.push_unchecked(co);
            }
            let old_co = unsafe { old.pop_unchecked() };
            run_coroutine(old_co);
            j += 1;
        }

        while j < size {
            let old_co = unsafe { old.pop_unchecked() };
            run_coroutine(old_co);
            j += 1;
        }
    }

    fn run(&self, id: usize) {
        let mut vec = StackVec::new();
        let mut cached = CacheBuf::new();
        loop {
            let mut total = 0;
            let mut size;

            // steal from normal thread visit list
            size = self.visit_list.bulk_pop_expect(id, 0, &mut vec);
            if size > 0 {
                total += size;
                self.run_coroutines(&mut vec, &mut cached, size);
            }

            // steal from the ready list
            size = self.ready_list.bulk_pop_expect(id, 0, &mut vec);
            if size > 0 {
                total += size;
                self.run_coroutines(&mut vec, &mut cached, size);
            }

            if total == 0 {
                let mut handle = thread::current();
                while let Err(h) = self.wait_list.push(handle) {
                    handle = h;
                }

                // do a re-check
                if !self.ready_list.is_empty() || self.visit_list.size() > 0 {
                    self.wait_list.pop().map(|t| t.unpark());
                }

                // thread::park_timeout(Duration::from_millis(100));
                thread::park();
            }
        }
    }

    /// put the coroutine to ready list so that next time it can be scheduled
    #[inline]
    pub fn schedule(&self, co: CoroutineImpl) {
        let id = ID.with(|m_id| m_id.get());
        if id == ID_INIT {
            self.visit_list.sp_push(co);
            self.wait_list.pop().map(|t| t.unpark());
            return;
        }

        self.ready_list.push(id, co);
        if self.ready_list.size() > BLOCK_SIZE / 2 {
            self.wait_list.pop().map(|t| t.unpark());
        }
    }

    #[inline]
    pub fn add_timer(&self,
                     dur: Duration,
                     co: Arc<AtomicOption<CoroutineImpl>>)
                     -> timeout_list::TimeoutHandle<TimerData> {
        self.timer_thread.add_timer(dur, co)
    }

    #[inline]
    pub fn del_timer(&self, handle: timeout_list::TimeoutHandle<TimerData>) {
        self.timer_thread.del_timer(handle);
    }

    #[inline]
    pub fn get_selector(&self) -> &Selector {
        self.event_loop.get_selector()
    }
}
