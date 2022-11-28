use std::fmt;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use crate::cancel::Cancel;
use crate::config::config;
use crate::join::{make_join_handle, Join, JoinHandle};
use crate::local::get_co_local_data;
use crate::local::CoroutineLocal;
use crate::park::Park;
use crate::scheduler::get_scheduler;
use crossbeam::atomic::AtomicCell;
use generator::{Generator, Gn};

/// /////////////////////////////////////////////////////////////////////////////
/// Coroutine framework types
/// /////////////////////////////////////////////////////////////////////////////

pub type EventResult = io::Error;

pub struct EventSubscriber {
    resource: *mut dyn EventSource,
}

// the EventSource is usually a heap obj that resides within the
// generator, it should be safe to send between threads.
unsafe impl Send for EventSubscriber {}

impl EventSubscriber {
    pub fn new(r: *mut dyn EventSource) -> Self {
        EventSubscriber { resource: r }
    }

    pub fn subscribe(self, c: CoroutineImpl) {
        let resource = unsafe { &mut *self.resource };
        resource.subscribe(c);
    }
}

pub trait EventSource {
    /// kernel handler of the event
    fn subscribe(&mut self, _c: CoroutineImpl);
    /// after yield back process
    fn yield_back(&self, cancel: &'static Cancel) {
        // after return back we should re-check the panic and clear it
        cancel.check_cancel();
    }
}

/// /////////////////////////////////////////////////////////////////////////////
/// Coroutine destruction
/// /////////////////////////////////////////////////////////////////////////////

pub struct Done;

impl Done {
    fn drop_coroutine(co: CoroutineImpl) {
        // assert!(co.is_done(), "unfinished coroutine detected");
        // just consume the coroutine
        // destroy the local storage
        let local = unsafe { Box::from_raw(get_co_local(&co)) };
        let name = local.get_co().name();

        // recycle the coroutine
        let (size, used) = co.stack_usage();
        if used == size {
            eprintln!("stack overflow detected, size={}", size);
            ::std::process::exit(1);
        }
        // show the actual used stack size in debug log
        if local.get_co().stack_size() & 1 == 1 {
            println!(
                "coroutine name = {:?}, stack size = {},  used size = {}",
                name, size, used
            );
        }

        if size == config().get_stack_size() {
            get_scheduler().pool.put(co);
        }
    }
}

impl EventSource for Done {
    fn subscribe(&mut self, co: CoroutineImpl) {
        Self::drop_coroutine(co);
    }
}

/// coroutines are static generator
/// the para type is EventResult, the result type is EventSubscriber
pub type CoroutineImpl = Generator<'static, EventResult, EventSubscriber>;

#[inline]
#[allow(clippy::cast_ptr_alignment)]
fn get_co_local(co: &CoroutineImpl) -> *mut CoroutineLocal {
    co.get_local_data() as *mut CoroutineLocal
}

/// /////////////////////////////////////////////////////////////////////////////
/// Coroutine
/// /////////////////////////////////////////////////////////////////////////////

/// The internal representation of a `Coroutine` handle
struct Inner {
    name: Option<String>,
    stack_size: usize,
    park: Park,
    cancel: Cancel,
}

#[derive(Clone)]
/// A handle to a coroutine.
pub struct Coroutine {
    inner: Arc<Inner>,
}

impl Coroutine {
    // Used only internally to construct a coroutine object without spawning
    fn new(name: Option<String>, stack_size: usize) -> Coroutine {
        Coroutine {
            inner: Arc::new(Inner {
                name,
                stack_size,
                park: Park::new(),
                cancel: Cancel::new(),
            }),
        }
    }

    /// Gets the coroutine stack size.
    pub fn stack_size(&self) -> usize {
        self.inner.stack_size
    }

    /// Atomically makes the handle's token available if it is not already.
    pub fn unpark(&self) {
        self.inner.park.unpark();
    }

    /// cancel a coroutine
    /// # Safety
    ///
    /// This function would force a coroutine exist when next scheduling
    /// And would drop all the resource tha the coroutine currently holding
    /// This may have unexpected side effects if you are not fully aware it
    pub unsafe fn cancel(&self) {
        self.inner.cancel.cancel();
    }

    /// Gets the coroutine name.
    pub fn name(&self) -> Option<&str> {
        self.inner.name.as_deref()
    }

    /// Get the internal cancel
    #[cfg(unix)]
    #[cfg(feature = "io_cancel")]
    pub(crate) fn get_cancel(&self) -> &Cancel {
        &self.inner.cancel
    }
}

impl fmt::Debug for Coroutine {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&self.name(), f)
    }
}

////////////////////////////////////////////////////////////////////////////////
// Builder
////////////////////////////////////////////////////////////////////////////////

/// Coroutine factory, which can be used in order to configure the properties of
/// a new coroutine.
///
/// Methods can be chained on it in order to configure it.
///
/// The two configurations available are:
///
/// - [`name`]: specifies an [associated name for the coroutine][naming-coroutines]
/// - [`stack_size`]: specifies the [desired stack size for the coroutine][stack-size]
///
/// The [`spawn`] method will take ownership of the builder and create an
/// `io::Result` to the coroutine handle with the given configuration.
///
/// The [`coroutine::spawn`] free function uses a `Builder` with default
/// configuration and `unwrap`s its return value.
///
/// You may want to use [`spawn`] instead of [`coroutine::spawn`], when you want
/// to recover from a failure to launch a coroutine, indeed the free function will
/// panics where the `Builder` method will return a `io::Result`.
///
/// # Examples
///
/// ```
/// use may::coroutine;
///
/// let builder = coroutine::Builder::new();
/// let code = || {
///     // coroutine code
/// };
///
/// let handler = unsafe { builder.spawn(code).unwrap() };
///
/// handler.join().unwrap();
/// ```
///
/// [`coroutine::spawn`]: ./fn.spawn.html
/// [`stack_size`]: ./struct.Builder.html#method.stack_size
/// [`name`]: ./struct.Builder.html#method.name
/// [`spawn`]: ./struct.Builder.html#method.spawn
/// [naming-coroutines]: ./index.html#naming-coroutine
/// [stack-size]: ./index.html#stack-siz
#[derive(Default)]
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

    /// Sets the size of the stack for the new coroutine.
    pub fn stack_size(mut self, size: usize) -> Builder {
        self.stack_size = Some(size);
        self
    }

    /// Spawns a new coroutine, and returns a join handle for it.
    /// The join handle can be used to block on
    /// termination of the child coroutine, including recovering its panics.
    fn spawn_impl<F, T>(self, f: F) -> io::Result<(CoroutineImpl, JoinHandle<T>)>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        static DONE: Done = Done {};

        let sched = get_scheduler();
        let Builder { name, stack_size } = self;
        let stack_size = stack_size.unwrap_or_else(|| config().get_stack_size());
        let _co = if stack_size == config().get_stack_size() {
            let co = sched.pool.get();
            co.prefetch();
            Some(co)
        } else {
            None
        };

        // create a join resource, shared by waited coroutine and *this* coroutine
        let panic = Arc::new(AtomicCell::new(None));
        let join = Arc::new(Join::new(panic.clone()));
        let packet = Arc::new(AtomicCell::new(None));
        let their_join = join.clone();
        let their_packet = packet.clone();

        let subscriber = EventSubscriber {
            resource: &DONE as &dyn EventSource as *const _ as *mut dyn EventSource,
        };

        let closure = move || {
            // trigger the JoinHandler
            // we must declare the variable before calling f so that stack is prepared
            // to unwind these local data. for the panic err we would set it in the
            // coroutine local data so that can return from the packet variable

            // set the return packet
            their_packet.swap(Some(f()));

            their_join.trigger();
            subscriber
        };

        let mut co = if let Some(mut c) = _co {
            // re-init the closure
            c.init_code(closure);
            c
        } else {
            Gn::new_opt(stack_size, closure)
        };

        let handle = Coroutine::new(name, stack_size);
        // create the local storage
        let local = CoroutineLocal::new(handle.clone(), join.clone());
        // attache the local storage to the coroutine
        co.set_local_data(Box::into_raw(local) as *mut u8);

        Ok((co, make_join_handle(handle, join, packet, panic)))
    }

    /// Spawns a new coroutine by taking ownership of the `Builder`, and returns an
    /// `io::Result` to its `JoinHandle`.
    ///
    /// The spawned coroutine may outlive the caller. The join handle can be used
    /// to block on termination of the child thread, including recovering its panics.
    ///
    /// # Errors
    ///
    /// Unlike the [`spawn`] free function, this method yields an
    /// `io::Result` to capture any failure to create the thread at
    /// the OS level.
    ///
    /// # Safety
    ///
    ///  - Access [`TLS`] in coroutine may trigger undefined behavior.
    ///  - If the coroutine exceed the stack during execution, this would trigger
    ///    memory segment fault
    ///
    /// If you find it annoying to wrap every thing in the unsafe block, you can
    /// use the [`go!`] macro instead.
    ///
    /// # Examples
    ///
    /// ```
    /// use may::coroutine;
    ///
    /// let builder = coroutine::Builder::new();
    ///
    /// let handler = unsafe {
    ///     builder.spawn(|| {
    ///         // thread code
    ///     }).unwrap()
    /// };
    ///
    /// handler.join().unwrap();
    /// ```
    ///
    /// [`TLS`]: ./index.html#TLS
    /// [`go!`]: ../macro.go.html
    /// [`spawn`]: ./fn.spawn.html
    pub unsafe fn spawn<F, T>(self, f: F) -> io::Result<JoinHandle<T>>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        // we will still get optimizations in spawn_impl
        let (co, handle) = self.spawn_impl(f)?;

        // put the coroutine to ready list
        get_scheduler().schedule_global(co);

        Ok(handle)
    }

    /// first run the coroutine in current thread, you should always use
    /// `spawn` instead of this API.
    ///
    /// # Safety
    ///
    /// Cancel would drop all the resource of the coroutine.
    /// Normally this is safe but for some cases you should
    /// take care of the side effect
    pub unsafe fn spawn_local<F, T>(self, f: F) -> io::Result<JoinHandle<T>>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        // we will still get optimizations in spawn_impl
        let (co, handle) = self.spawn_impl(f)?;
        // first run the coroutine in current thread
        run_coroutine(co);
        Ok(handle)
    }
}

////////////////////////////////////////////////////////////////////////////////
// Free functions
////////////////////////////////////////////////////////////////////////////////

/// Spawns a new coroutine, returning a [`JoinHandle`] for it.
///
/// The join handle will implicitly *detach* the child coroutine upon being
/// dropped. In this case, the child coroutine may outlive the parent.
/// Additionally, the join handle provides a [`join`] method that can be used
/// to join the child coroutine. If the child coroutine panics, [`join`] will
/// return an `Err` containing the argument given to `panic`.
///
/// This will create a coroutine using default parameters of [`Builder`], if you
/// want to specify the stack size or the name of the coroutine, use this API
/// instead.
///
/// This API has the same semantic as the `std::thread::spawn` API, except that
/// it is an unsafe method.
///
/// # Safety
///
///  - Access [`TLS`] in coroutine may trigger undefined behavior.
///  - If the coroutine exceed the stack during execution, this would trigger
///    memory segment fault
///
/// If you find it annoying to wrap every thing in the unsafe block, you can
/// use the [`go!`] macro instead.
///
/// # Examples
///
/// Creating a coroutine.
///
/// ```
/// use may::coroutine;
///
/// let handler = unsafe {
///     coroutine::spawn(|| {
///         // coroutine code
///     })
/// };
///
/// handler.join().unwrap();
/// ```
///
/// [`TLS`]: ./index.html#TLS
/// [`go!`]: ../macro.go.html
/// [`JoinHandle`]: struct.JoinHandle.html
/// [`join`]: struct.JoinHandle.html#method.join
/// [`Builder::spawn`]: struct.Builder.html#method.spawn
/// [`Builder`]: struct.Builder.html
pub unsafe fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    Builder::new().spawn(f).unwrap()
}

/// Gets a handle to the coroutine that invokes it.
/// it will panic if you call it in a thread context
#[inline]
pub fn current() -> Coroutine {
    match get_co_local_data() {
        Some(local) => unsafe { local.as_ref() }.get_co().clone(),
        None => panic!("no current coroutine, did you call `current()` in thread context?"),
    }
}

/// if current context is coroutine
#[inline]
pub fn is_coroutine() -> bool {
    // we never call this function in a pure generator context
    // so we can sure that this function is called
    // either in a thread context or in a coroutine context
    get_co_local_data().is_some()
}

/// get current coroutine cancel registration
/// panic in a thread context
#[inline]
pub(crate) fn current_cancel_data() -> &'static Cancel {
    match get_co_local_data() {
        Some(local) => &(unsafe { &*local.as_ptr() }.get_co().inner.cancel),
        None => panic!("no cancel data, did you call `current_cancel_data()` in thread context?"),
    }
}

#[inline]
pub(crate) fn co_cancel_data(co: &CoroutineImpl) -> &'static Cancel {
    let local = unsafe { &*get_co_local(co) };
    &local.get_co().inner.cancel
}

// windows use delay drop instead
#[cfg(unix)]
#[cfg(feature = "io_cancel")]
pub(crate) fn co_get_handle(co: &CoroutineImpl) -> Coroutine {
    let local = unsafe { &*get_co_local(co) };
    local.get_co().clone()
}

/// timeout block the current coroutine until it's get unparked
#[inline]
fn park_timeout_impl(dur: Option<Duration>) {
    if !is_coroutine() {
        // in thread context we do nothing
        return;
    }

    let co_handle = current();
    co_handle.inner.park.park_timeout(dur).ok();
}

/// block the current coroutine until it's get unparked
pub fn park() {
    park_timeout_impl(None);
}

/// timeout block the current coroutine until it's get unparked
pub fn park_timeout(dur: Duration) {
    park_timeout_impl(Some(dur));
}

/// run the coroutine
#[inline]
pub(crate) fn run_coroutine(mut co: CoroutineImpl) {
    match co.resume() {
        Some(ev) => ev.subscribe(co),
        None => {
            // panic happened here
            let local = unsafe { &mut *get_co_local(&co) };
            let join = local.get_join();
            // set the panic data
            if let Some(panic) = co.get_panic_data() {
                join.set_panic_data(panic);
            }
            // trigger the join here
            join.trigger();
            Done::drop_coroutine(co);
        }
    }
}
