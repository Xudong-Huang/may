extern crate generator;
extern crate queue;

use generator::Generator;

mod scheduler;
mod yield_out;
pub use scheduler::Scheduler;
pub use yield_out::yield_out;

pub struct EventResult {
}

pub struct EventSubscriber {
    resource: *mut EventSource,
}

impl EventSubscriber {
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
