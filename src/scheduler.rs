use std::thread;
use std::sync::{Once, ONCE_INIT};
use queue::{BulkPop, BLOCK_SIZE};
use queue::wait_queue::Queue as CoQueue;
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
        // run the scheduler in background
        for _i in 0..2 {
            thread::spawn(move || {
                let s = unsafe { &*sched };
                s.run();
            });
        }
    });
    unsafe { &*sched }
}

pub struct Scheduler {
    ready_list: CoQueue<Coroutine>,
}

impl Scheduler {
    pub fn new() -> Box<Self> {
        Box::new(Scheduler { ready_list: CoQueue::new() })
    }

    #[allow(dead_code)]
    pub fn run(&self) {
        let mut vec = Vec::with_capacity(BLOCK_SIZE);
        loop {
            let size = self.ready_list.bulk_pop(&mut vec);
            vec.drain(0..size).fold((), |_, mut coroutine| {
                let event_subscriber = coroutine.resume().unwrap();
                // the coroutine will sink into the subscriber's resouce
                // basically it just register the coroutine somewhere within the 'resource'
                // 'resource' is just any type that live within the coroutine's stack
                event_subscriber.subscribe(coroutine);
            });
        }
    }

    // this function should be thread safe, that multi thread running on
    #[allow(dead_code)]
    pub fn run_to_complete(&self) {
        let mut vec = Vec::with_capacity(BLOCK_SIZE);
        loop {
            let size = self.ready_list.bulk_pop_expect(0, &mut vec);
            if size == 0 {
                break;
            }
            vec.drain(0..size).fold((), |_, mut coroutine| {
                let event_subscriber = coroutine.resume().unwrap();
                // the coroutine will sink into the subscriber's resouce
                // basically it just register the coroutine somewhere within the 'resource'
                // 'resource' is just any type that live within the coroutine's stack
                event_subscriber.subscribe(coroutine);
            });
        }
    }

    /// put the coroutine to ready list so that next time it can be scheduled
    #[inline]
    pub fn schedule(&self, co: Coroutine) {
        self.ready_list.push(co);
    }
}
