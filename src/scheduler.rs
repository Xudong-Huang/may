use std::io;
// use std::cmp;
use std::thread;
use std::cell::Cell;
use std::time::Duration;
use std::intrinsics::likely;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Once, ONCE_INIT};
use smallvec::SmallVec;
use queue::BLOCK_SIZE;
use queue::mpmc::Queue as mpmc;
use queue::spmc::Queue as spmc;
use queue::mpmc_v1::Queue as generic_mpmc;
use queue::mpmc_bounded::Queue as WaitList;
use queue::stack_ringbuf::RingBuf;
use coroutine::CoroutineImpl;
use io::Selector;
use timeout_list;
use sync::BoxedOption;
use pool::CoroutinePool;
use io::{EventLoop, EventData};
use yield_now::set_co_para;

const ID_INIT: usize = ::std::usize::MAX;
thread_local!{static ID: Cell<usize> = Cell::new(ID_INIT);}

static mut WORKERS: usize = 1;
const PREFTCH_SIZE: usize = 4;

/// set the worker thread number
/// this function should be called at the program beginning
/// successive call would not tack effect for that the scheduler
/// is already started
pub fn scheduler_set_workers(workers: usize) {
    info!("set workers={:?}", workers);
    unsafe {
        WORKERS = workers;
    }
    get_scheduler();
}

// here we use Arc<BoxedOption<>> for that in the select implementaion
// other event may try to consume the coroutine while timer thread consume it
type TimerData = Arc<BoxedOption<CoroutineImpl>>;
type TimerList = timeout_list::TimeOutList<TimerData>;

#[inline]
pub fn get_scheduler() -> &'static Scheduler {
    static mut sched: *const Scheduler = 0 as *const _;
    unsafe {
        if likely(!sched.is_null()) {
            return &*sched;
        }
    }

    static ONCE: Once = ONCE_INIT;
    ONCE.call_once(|| {
        let workers = unsafe { WORKERS };
        let b: Box<Scheduler> = Scheduler::new(workers);
        unsafe {
            sched = Box::into_raw(b);
        }

        // run the workers in background
        for id in 0..workers {
            thread::spawn(move || {
                ID.with(|m_id| m_id.set(id));
                let s = unsafe { &*sched };
                s.run(id);
            });
        }

        // timer thread
        thread::spawn(move || {
            let s = unsafe { &*sched };
            // timer function
            let timer_event_handler = |co: Arc<BoxedOption<CoroutineImpl>>| {
                // just repush the co to the visit list
                co.take_fast(Ordering::Relaxed).map(|mut c| {
                    // set the timeout result for the coroutine
                    set_co_para(&mut c, io::Error::new(io::ErrorKind::TimedOut, "timeout"));
                    s.schedule(c);
                });
            };

            s.timer_list.run(&timer_event_handler);
        });

        // io event loop thread
        thread::spawn(move || {
            let s = unsafe { &*sched };
            s.event_loop.run().unwrap();
        });
    });
    unsafe { &*sched }
}

pub struct Scheduler {
    pub pool: CoroutinePool,
    event_loop: EventLoop,
    event_list: spmc<CoroutineImpl>,
    ready_list: mpmc<CoroutineImpl>,
    visit_list: generic_mpmc<CoroutineImpl>,
    timer_list: TimerList,
    wait_list: WaitList<thread::Thread>, // workers: usize,
}

type StackVec = SmallVec<[CoroutineImpl; BLOCK_SIZE]>;
type CacheBuf = RingBuf<[CoroutineImpl; PREFTCH_SIZE + 2]>;

impl Scheduler {
    pub fn new(workers: usize) -> Box<Self> {
        Box::new(Scheduler {
            pool: CoroutinePool::new(),
            event_loop: EventLoop::new().unwrap(),
            event_list: spmc::new(workers),
            ready_list: mpmc::new(workers),
            timer_list: TimerList::new(),
            visit_list: generic_mpmc::new(workers),
            wait_list: WaitList::with_capacity(256), // workers: workers,
        })
    }

    // TODO: use local stack ring buf
    #[inline]
    fn run_coroutines(&self, vec: &mut StackVec, old: &mut CacheBuf, size: usize) {
        let mut drain = vec.drain();
        // prefech one coroutine
        let mut j = 0;
        while j < PREFTCH_SIZE && j < size {
            let co = drain.next().unwrap();
            co.prefetch();
            unsafe {
                old.push_unchecked(co);
            }
            j += 1;
        }

        j = 0;
        while (j as isize) < (size as isize) - (PREFTCH_SIZE as isize) {
            let co = drain.next().unwrap();
            co.prefetch();
            unsafe {
                old.push_unchecked(co);
            }
            let mut old_co = unsafe { old.pop_unchecked() };
            let event_subscriber = old_co.resume().unwrap();
            event_subscriber.subscribe(old_co);
            j += 1;
        }

        while j < size {
            let mut old_co = unsafe { old.pop_unchecked() };
            let event_subscriber = old_co.resume().unwrap();
            event_subscriber.subscribe(old_co);
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
            // let mut size = self.visit_list.size();
            // if size > 0 {
            // size = cmp::max(size / self.workers, 1);
            size = self.visit_list.bulk_pop_expect(id, 0, &mut vec);
            if size > 0 {
                total += size;
                self.run_coroutines(&mut vec, &mut cached, size);
            }
            // }

            // steal from io event list
            // let mut size = self.event_list.size();
            // if size > 0 {
            // size = cmp::max(size / self.workers, 1);
            // size = self.event_list.bulk_pop_expect(id, size, &mut vec);
            size = self.event_list.bulk_pop_expect(id, 0, &mut vec);
            if size > 0 {
                total += size;
                self.run_coroutines(&mut vec, &mut cached, size);
            }
            // }

            // steal from the ready list
            // let mut size = self.ready_list.size();
            // if size > 0 {
            // size = cmp::max(size / self.workers, 1);
            size = self.ready_list.bulk_pop_expect(id, 0, &mut vec);
            if size > 0 {
                total += size;
                self.run_coroutines(&mut vec, &mut cached, size);
            }
            // }

            if total == 0 {
                let mut handle = thread::current();
                loop {
                    match self.wait_list.push(handle) {
                        Ok(_) => {
                            break;
                        }
                        Err(h) => {
                            handle = h;
                        }
                    }
                }

                if !self.ready_list.is_empty() || self.visit_list.size() > 0 ||
                   self.event_list.size() > 0 {
                    self.wait_list.pop().map(|t| t.unpark());
                }

                thread::park();
            }
        }
    }

    /// put the coroutine to ready list so that next time it can be scheduled
    #[inline]
    pub fn schedule(&self, co: CoroutineImpl) {
        let id = ID.with(|m_id| m_id.get());
        if id != ID_INIT {
            self.ready_list.push(id, co);
        } else {
            self.visit_list.push(co);
        }
        // signal one waiting thread if any
        self.wait_list.pop().map(|t| t.unpark());
    }

    #[inline]
    pub fn schedule_io(&self, co: CoroutineImpl) {
        self.event_list.push(co);
        // signal one waiting thread if any
        self.wait_list.pop().map(|t| t.unpark());
    }

    #[inline]
    pub fn add_timer(&self, dur: Duration, co: Arc<BoxedOption<CoroutineImpl>>) {
        self.timer_list.add_timer(dur, co);
    }

    #[inline]
    pub fn add_io_timer(&self, event: &mut EventData, timeout: Option<Duration>) {
        self.event_loop.add_io_timer(event, timeout);
    }

    #[inline]
    pub fn get_selector(&self) -> &Selector {
        self.event_loop.get_selector()
    }
}
