use std::sync::{Once, ONCE_INIT};
use queue::{BulkPop, BLOCK_SIZE};
use queue::mpmc::Queue as CoQueue;
use coroutine::Coroutine;

#[inline]
pub fn get_scheduler() -> &'static Scheduler {
    static mut sched: *const Scheduler = 0 as *const _;
    static ONCE: Once = ONCE_INIT;
    ONCE.call_once(|| {
        let b: Box<Scheduler> = Scheduler::new();
        unsafe {
            sched = Box::into_raw(b);
        }
    });
    unsafe { &*sched }
}

#[inline]
pub fn sched_run() {
    get_scheduler().run_to_complete();
}


pub struct Scheduler {
    ready_list: CoQueue<Coroutine>,
}

impl Scheduler {
    pub fn new() -> Box<Self> {
        Box::new(Scheduler { ready_list: CoQueue::new() })
    }

    pub fn run_to_complete(&self) {
        let mut vec = Vec::with_capacity(BLOCK_SIZE);
        loop {
            let size = self.ready_list.bulk_pop(&mut vec);
            if size == 0 {
                break;
            }
            vec.drain(0..size)
                .fold((), |_, mut coroutine| {
                    let event_subscriber = coroutine.resume().unwrap();
                    // the coroutine will sink into the subscriber's resouce
                    // basically it just register the coroutine somewhere within the 'resource'
                    // 'resource' is just any type that live within the coroutine's stack
                    event_subscriber.subscribe(coroutine);
                });
        }
    }

    // pub fn start(&self, c: Coroutine) {
    //     self.ready_list.push(c);
    //     self.run_to_complete();
    // }

    /// put the coroutine to ready list so that next time it can be scheduled
    #[inline]
    pub fn schedule(&self, co: Coroutine) {
        self.ready_list.push(co);
    }
}
