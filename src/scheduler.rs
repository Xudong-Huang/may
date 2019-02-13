use std::io;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Once, ONCE_INIT};
use std::thread;
use std::time::Duration;

use config::config;
use coroutine_impl::{run_coroutine, CoroutineImpl};
use crossbeam::channel;
use io::{EventLoop, Selector};
use pool::CoroutinePool;
use sync::AtomicOption;
use timeout_list;
use yield_now::set_co_para;

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
    use generator::Error;
    use std::panic;
    let old = panic::take_hook();
    ::std::panic::set_hook(Box::new(move |info| {
        if let Some(&Error::Cancel) = info.payload().downcast_ref::<Error>() {
            // this is not an error at all, ignore it
            return;
        }
        old(info);
    }));
}

static mut SCHED: *const Scheduler = 0 as *const _;

#[cold]
#[inline(never)]
fn init_scheduler() {
    let workers = config().get_workers();
    let mut io_workers = config().get_io_workers();
    let run_on_io = if io_workers == 0 {
        // running all the coroutines on normal worker thread
        io_workers = 1;
        false
    } else {
        true
    };
    let b: Box<Scheduler> = Scheduler::new(io_workers, run_on_io);
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
            if let Some(mut c) = co.take_fast(Ordering::Relaxed) {
                // set the timeout result for the coroutine
                set_co_para(&mut c, io::Error::new(io::ErrorKind::TimedOut, "timeout"));
                s.schedule(c);
            }
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
}

#[inline]
pub fn get_scheduler() -> &'static Scheduler {
    unsafe {
        if likely(!SCHED.is_null()) {
            return &*SCHED;
        }
    }

    static ONCE: Once = ONCE_INIT;
    ONCE.call_once(init_scheduler);
    unsafe { &*SCHED }
}

pub struct Scheduler {
    pub pool: CoroutinePool,
    event_loop: EventLoop,
    rx: channel::Receiver<CoroutineImpl>,
    tx: channel::Sender<CoroutineImpl>,
    timer_thread: TimerThread,
}

impl Scheduler {
    pub fn new(io_workers: usize, run_on_io: bool) -> Box<Self> {
        let (tx, rx) = channel::unbounded();
        Box::new(Scheduler {
            pool: CoroutinePool::new(),
            event_loop: EventLoop::new(io_workers, run_on_io).expect("can't create event_loop"),
            tx,
            rx,
            timer_thread: TimerThread::new(),
        })
    }

    fn run(&self) {
        for co in &self.rx {
            run_coroutine(co);
        }
    }

    /// put the coroutine to ready list so that next time it can be scheduled
    #[inline]
    pub fn schedule(&self, co: CoroutineImpl) {
        self.tx
            .send(co)
            .expect("failed to send coroutine to scheduler");
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
