use std::cmp;
use std::thread;
use std::cell::Cell;
use std::sync::{Once, ONCE_INIT};
use queue::BLOCK_SIZE;
use queue::mpmc_v1::Queue as generic_mpmc;
use queue::mpmc::Queue as mpmc;
use queue::mpmc_bounded::Queue as WaitList;
use coroutine::CoroutineImpl;
use pool::CoroutinePool;

const ID_INIT: usize = ::std::usize::MAX;
thread_local!{static ID: Cell<usize> = Cell::new(ID_INIT);}

#[inline]
pub fn get_scheduler() -> &'static Scheduler {
    let workers = 4;
    static mut sched: *const Scheduler = 0 as *const _;
    static ONCE: Once = ONCE_INIT;
    ONCE.call_once(|| {
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
    });
    unsafe { &*sched }
}

pub struct Scheduler {
    pub pool: CoroutinePool,
    ready_list: mpmc<CoroutineImpl>,
    visit_list: generic_mpmc<CoroutineImpl>,
    wait_list: WaitList<thread::Thread>,
    workers: usize,
}

impl Scheduler {
    pub fn new(workers: usize) -> Box<Self> {
        Box::new(Scheduler {
            pool: CoroutinePool::new(),
            // ready_list: (0..workers).map(|_| spsc::new()).collect(),
            // timer_list: mpsc::new(workers),
            // event_list: mpsc::new(workers),
            ready_list: mpmc::new(workers),
            visit_list: generic_mpmc::new(workers),
            wait_list: WaitList::with_capacity(256),
            // cnt: AtomicUsize::new(0),
            workers: workers,
        })
    }

    fn run_coroutines(&self, vec: &mut Vec<CoroutineImpl>) {
        for coroutine in vec.iter() {
            coroutine.prefetch();
        }

        vec.drain(..).fold((), |_, mut coroutine| {
            let event_subscriber = coroutine.resume().unwrap();
            // the coroutine will sink into the subscriber's resouce
            // basically it just register the coroutine somewhere within the 'resource'
            // 'resource' is just any type that live within the coroutine's stack
            event_subscriber.subscribe(coroutine);
        });
    }

    fn run(&self, id: usize) {
        let mut vec = Vec::with_capacity(BLOCK_SIZE);
        loop {
            let mut total = 0;

            // steal from normal thread list
            let mut size = self.visit_list.size();
            if size > 0 {
                size = cmp::max(size / self.workers, 1);
                size = self.visit_list.bulk_pop_expect(id, size, &mut vec);
                if size > 0 {
                    total += size;
                    self.run_coroutines(&mut vec);
                }
            }

            // steal from the sapwn list
            let mut size = self.ready_list.size();
            if size > 0 {
                size = cmp::max(size / self.workers, 1);
                size = self.ready_list.bulk_pop_expect(id, size, &mut vec);
                if size > 0 {
                    total += size;
                    self.run_coroutines(&mut vec);
                }
            }

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

                if !self.ready_list.is_empty() || self.visit_list.size() > 0 {
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
}
