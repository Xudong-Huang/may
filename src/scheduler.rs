use std::io;
use std::thread;
use std::time::Duration;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Once, ONCE_INIT};

use timeout_list;
use config::config;
use sync::AtomicOption;
use pool::CoroutinePool;
use yield_now::set_co_para;
use io::{EventLoop, Selector};
use crossbeam::sync::SegQueue as mpmc;
use may_queue::mpmc_bounded::Queue as WaitList;
use coroutine_impl::{run_coroutine, CoroutineImpl};

#[cfg(nightly)]
use std::intrinsics::likely;
#[cfg(not(nightly))]
#[inline]
fn likely(e: bool) -> bool {
    e
}

// here we use Arc<AtomicOption<>> for that in the select implementation
// other event may try to consume the coroutine while timer thread consume it
type TimerData = Arc<AtomicOption<CoroutineImpl>>;
type TimerThread = timeout_list::TimerThread<TimerData>;

// filter out the cancel panic, don't print anything for it
fn filter_cancel_panic() {
    use std::panic;
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
        let workers = config().get_workers();
        let io_workers = config().get_io_workers();
        let b: Box<Scheduler> = Scheduler::new(io_workers);
        unsafe {
            SCHED = Box::into_raw(b);
        }

        // run the workers in background
        for _id in 0..workers {
            thread::spawn(move || {
                filter_cancel_panic();
                let s = unsafe { &*SCHED };
                s.run();
            });
        }

        // timer thread
        thread::spawn(move || {
            filter_cancel_panic();
            let s = unsafe { &*SCHED };
            // timer function
            let timer_event_handler = |co: Arc<AtomicOption<CoroutineImpl>>| {
                // just re-push the co to the visit list
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
    wait_list: WaitList<thread::Thread>,
    timer_thread: TimerThread,
}

impl Scheduler {
    pub fn new(io_workers: usize) -> Box<Self> {
        Box::new(Scheduler {
            pool: CoroutinePool::new(),
            event_loop: EventLoop::new(io_workers).expect("can't create event_loop"),
            ready_list: mpmc::new(),
            timer_thread: TimerThread::new(),
            wait_list: WaitList::with_capacity(256), // workers: workers,
        })
    }

    fn run(&self) {
        loop {
            // steal from the ready list
            if let Some(co) = self.ready_list.try_pop() {
                run_coroutine(co);
                continue;
            }

            // first register thread handle
            let mut handle = thread::current();
            while let Err(h) = self.wait_list.push(handle) {
                handle = h;
            }

            // do a re-check
            if !self.ready_list.is_empty() {
                self.wait_list.pop().map(|t| t.unpark());
            }

            // thread::park_timeout(Duration::from_millis(100));
            thread::park();
        }
    }

    /// put the coroutine to ready list so that next time it can be scheduled
    #[inline]
    pub fn schedule(&self, co: CoroutineImpl) {
        self.ready_list.push(co);
        // signal one waiting thread if any
        self.wait_list.pop().map(|t| t.unpark());
    }

    #[inline]
    pub fn add_timer(
        &self,
        dur: Duration,
        co: Arc<AtomicOption<CoroutineImpl>>,
    ) -> timeout_list::TimeoutHandle<TimerData> {
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
