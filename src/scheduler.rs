use generator::Gn;
use queue::{BulkPop, BLOCK_SIZE};
use queue::mpmc::Queue as CoQueue;
use super::{Coroutine, EventSource, EventSubscriber};

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

    pub fn make_co<F: FnOnce() + 'static>(&self, f: F) -> Coroutine {
        struct Done;
        impl EventSource for Done {
            fn subscribe(&mut self, _co: Coroutine) {
                // just consume the coroutine
                // println!("done()");
            }
        }

        static DONE: Done = Done {};
        let done = &DONE as &EventSource as *const _ as *mut EventSource;
        let mut g = Gn::new(move || {
            f();
            EventSubscriber { resource: done }
        });

        {
            // this will set the scheduler as the context extra data
            let context = g.get_context();
            context.extra = self as *const _ as *mut _;
        }

        g
    }

    pub fn spawn<F: FnOnce() + 'static>(&self, f: F) {
        let co = self.make_co(f);
        // put the coroutine to ready list
        self.schedule(co);
    }
}
