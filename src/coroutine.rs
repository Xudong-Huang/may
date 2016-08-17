use generator::{Gn, Generator};
use scheduler::get_scheduler;

pub struct EventResult {
}

pub struct EventSubscriber {
    resource: *mut EventSource,
}

impl EventSubscriber {
    pub fn new(r: *mut EventSource) -> Self {
        EventSubscriber { resource: r }
    }

    pub fn subscribe(self, c: Coroutine) {
        let resource = unsafe { &mut *self.resource };
        resource.subscribe(c);
    }
}

pub trait EventSource {
    /// this will trigger an event and cause the registered coroutine push into the 'ready list'
    fn trigger(&mut self) {}
    /// register a coroutine waiting on the resource
    fn subscribe(&mut self, _c: Coroutine) {}
}

// coroutines are static generator, the para type is EventResult, the result type is EventSubscriber
pub type Coroutine = Generator<'static, EventResult, EventSubscriber>;

fn make_co<F: FnOnce() + 'static>(f: F) -> Coroutine {
    struct Done;
    impl EventSource for Done {
        fn subscribe(&mut self, _co: Coroutine) {
            // just consume the coroutine
            // println!("done()");
        }
    }

    static DONE: Done = Done {};
    let done = &DONE as &EventSource as *const _ as *mut EventSource;
    Gn::new(move || {
        f();
        EventSubscriber { resource: done }
    })
}

pub fn spawn<F: FnOnce() + 'static>(f: F) {
    let co = make_co(f);
    // put the coroutine to ready list
    get_scheduler().schedule(co);
}
