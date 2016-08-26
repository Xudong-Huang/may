use std::sync::Arc;
use std::cell::{UnsafeCell, RefCell};
use park::Park;
use generator::{Gn, Generator, get_local_data};
use scheduler::get_scheduler;
use join::{Join, JoinHandle, make_join_handle};
use local::CoroutineLocal;
use yield_now::yield_with;


// default stack size, in usize
// windows has a minimal size as 0x4a8!!!!
pub const DEFAULT_STACK_SIZE: usize = 0x800;

/// /////////////////////////////////////////////////////////////////////////////
/// Coroutine framework types
/// /////////////////////////////////////////////////////////////////////////////

pub struct EventResult {
}

pub struct EventSubscriber {
    resource: *mut EventSource,
}

impl EventSubscriber {
    pub fn new(r: *mut EventSource) -> Self {
        EventSubscriber { resource: r }
    }

    pub fn subscribe(self, c: CoroutineImpl) {
        let resource = unsafe { &mut *self.resource };
        resource.subscribe(c);
    }
}

pub trait EventSource {
    /// this will trigger an event and cause the registered coroutine push into the 'ready list'
    fn trigger(&mut self) {}
    /// register a coroutine waiting on the resource
    fn subscribe(&mut self, _c: CoroutineImpl) {}
}

/// /////////////////////////////////////////////////////////////////////////////
/// Coroutine destruction
/// /////////////////////////////////////////////////////////////////////////////

struct Done;

impl EventSource for Done {
    fn subscribe(&mut self, co: CoroutineImpl) {
        // assert!(co.is_done(), "unfinished coroutine detected");
        // just consume the coroutine
        // destroy the local storage
        unsafe {
            Box::from_raw(co.get_local_data() as *mut CoroutineLocal);
        }
    }
}

/// coroutines are static generator, the para type is EventResult, the result type is EventSubscriber
pub type CoroutineImpl = Generator<'static, EventResult, EventSubscriber>;

/// /////////////////////////////////////////////////////////////////////////////
/// Coroutine
/// /////////////////////////////////////////////////////////////////////////////

/// The internal representation of a `Coroutine` handle
struct Inner {
    name: Option<String>,
    park: Park,
}

#[derive(Clone)]
/// A handle to a coroutine.
pub struct Coroutine {
    inner: Arc<Inner>,
}

impl Coroutine {
    // Used only internally to construct a coroutine object without spawning
    fn new(name: Option<String>) -> Coroutine {
        Coroutine {
            inner: Arc::new(Inner {
                name: name,
                park: Park::new(),
            }),
        }
    }

    /// Atomically makes the handle's token available if it is not already.
    pub fn unpark(&self) {
        self.inner.park.unpark();
    }

    /// Gets the coroutine's name.
    pub fn name(&self) -> Option<&str> {
        self.inner.name.as_ref().map(|s| &**s)
    }
}


/// /////////////////////////////////////////////////////////////////////////////
/// Builder
/// /////////////////////////////////////////////////////////////////////////////

/// Coroutine configuration. Provides detailed control over the properties
/// and behavior of new coroutines.
pub struct Builder {
    // A name for the coroutine-to-be, for identification in panic messages
    name: Option<String>,
    // The size of the stack for the spawned coroutine
    stack_size: Option<usize>,
}

impl Builder {
    /// Generates the base configuration for spawning a coroutine, from which
    /// configuration methods can be chained.
    pub fn new() -> Builder {
        Builder {
            name: None,
            stack_size: None,
        }
    }

    /// Names the thread-to-be. Currently the name is used for identification
    /// only in panic messages.
    pub fn name(mut self, name: String) -> Builder {
        self.name = Some(name);
        self
    }

    /// Sets the size of the stack for the new thread.
    pub fn stack_size(mut self, size: usize) -> Builder {
        self.stack_size = Some(size);
        self
    }

    /// Spawns a new coroutine, and returns a join handle for it.
    /// The join handle can be used to block on
    /// termination of the child coroutine, including recovering its panics.
    pub fn spawn<F, T>(self, f: F) -> JoinHandle<T>
        where F: FnOnce() -> T,
              F: Send + 'static,
              T: Send + 'static
    {
        static DONE: Done = Done {};
        let done = &DONE as &EventSource as *const _ as *mut EventSource;
        let Builder { name, stack_size } = self;
        let stack_size = stack_size.unwrap_or(DEFAULT_STACK_SIZE);
        // create a join resource, shared by waited coroutine and *this* coroutine
        let join = Arc::new(UnsafeCell::new(Join::new()));
        let packet = Arc::new(RefCell::new(None));
        let their_join = join.clone();
        let their_packet = packet.clone();
        let mut co = Gn::new_opt(stack_size, move || {
            *their_packet.borrow_mut() = Some(f());
            // trigger the JoinHandler
            let join = unsafe { &mut *their_join.get() };
            join.trigger();
            EventSubscriber { resource: done }
        });


        let handle = Coroutine::new(name);
        // create the local storage
        let local = CoroutineLocal::new(handle.clone());
        // attache the local storage to the coroutine
        co.set_local_data(Box::into_raw(local) as *mut u8);

        // put the coroutine to ready list
        get_scheduler().schedule(co);
        make_join_handle(handle, join, packet)
    }
}

/// /////////////////////////////////////////////////////////////////////////////
/// Free functions
/// /////////////////////////////////////////////////////////////////////////////
pub fn spawn<F, T>(f: F) -> JoinHandle<T>
    where F: FnOnce() -> T + Send + 'static,
          T: Send + 'static
{
    Builder::new().spawn(f)
}

/// Gets a handle to the thread that invokes it.
#[inline]
pub fn current() -> Coroutine {
    let local = unsafe { &*(get_local_data() as *mut CoroutineLocal) };
    local.get_co()
}

/// block the current coroutine until it's get unparked
pub fn park() {
    let co_handle = current();
    let park = &co_handle.inner.park;

    // if the state is not set, need to wait
    if park.need_park() {
        // what if the state is set before yield?
        // the subscribe would re-check it
        yield_with(park);
    }
}
