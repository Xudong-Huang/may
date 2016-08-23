// modified from crossbeam

use std::cell::RefCell;
use std::fmt;
use std::mem;
use std::rc::Rc;
use std::sync::Arc;

use spawn;
use join::JoinHandle;

struct SyncCell<T>(pub RefCell<T>);
unsafe impl<T: Send> Send for SyncCell<T> {}
unsafe impl<T: Send> Sync for SyncCell<T> {}

#[doc(hidden)]
trait FnBox {
    fn call_box(self: Box<Self>);
}

impl<F: FnOnce()> FnBox for F {
    fn call_box(self: Box<Self>) {
        (*self)()
    }
}

/// Like `coroutine::spawn`, but without the closure bounds.
pub unsafe fn spawn_unsafe<'a, F>(f: F) -> JoinHandle<()>
    where F: FnOnce() + Send + 'a
{
    let closure: Box<FnBox + 'a> = Box::new(f);
    let closure: Box<FnBox + Send> = mem::transmute(closure);
    spawn(move || closure.call_box())
}


pub struct Scope<'a> {
    dtors: RefCell<Option<DtorChain<'a>>>,
}

struct DtorChain<'a> {
    dtor: Box<FnBox + 'a>,
    next: Option<Box<DtorChain<'a>>>,
}

enum JoinState {
    Running(JoinHandle<()>),
    Joined,
}

impl JoinState {
    fn join(&mut self) {
        let mut state = JoinState::Joined;
        mem::swap(self, &mut state);
        if let JoinState::Running(handle) = state {
            handle.join();
        }
    }
}

/// A handle to a scoped coroutine
pub struct ScopedJoinHandle<T> {
    inner: Rc<RefCell<JoinState>>,
    packet: Arc<SyncCell<Option<T>>>,
}

/// Create a new `scope`, for deferred destructors.
///
/// Scopes, in particular, support scoped coroutine spawning.
///
/// # Examples
///
/// Creating and using a scope:
///
/// ```
/// coroutine::scope(|scope| {
///     scope.defer(|| println!("Exiting scope"));
///     scope.spawn(|| println!("Running child coroutine in scope"))
/// });
/// // Prints messages in the reverse order written
/// ```
pub fn scope<'a, F, R>(f: F) -> R
    where F: FnOnce(&Scope<'a>) -> R
{
    let mut scope = Scope { dtors: RefCell::new(None) };
    let ret = f(&scope);
    scope.drop_all();
    ret
}

impl<'a> fmt::Debug for Scope<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Scope {{ ... }}")
    }
}

impl<T> fmt::Debug for ScopedJoinHandle<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "ScopedJoinHandle {{ ... }}")
    }
}

impl<'a> Scope<'a> {
    // This method is carefully written in a transactional style, so
    // that it can be called directly and, if any dtor panics, can be
    // resumed in the unwinding this causes. By initially running the
    // method outside of any destructor, we avoid any leakage problems
    // due to @rust-lang/rust#14875.
    fn drop_all(&mut self) {
        loop {
            // use a separate scope to ensure that the RefCell borrow
            // is relinquishe before running `dtor`
            let dtor = {
                let mut dtors = self.dtors.borrow_mut();
                if let Some(mut node) = dtors.take() {
                    *dtors = node.next.take().map(|b| *b);
                    node.dtor
                } else {
                    return;
                }
            };
            dtor.call_box();
        }
    }

    /// Schedule code to be executed when exiting the scope.
    ///
    /// This is akin to having a destructor on the stack, except that it is
    /// *guaranteed* to be run.
    pub fn defer<F>(&self, f: F)
        where F: FnOnce() + 'a
    {
        let mut dtors = self.dtors.borrow_mut();
        *dtors = Some(DtorChain {
            dtor: Box::new(f),
            next: dtors.take().map(Box::new),
        });
    }

    /// Create a scoped coroutine.
    ///
    /// `spawn` is similar to the `spawn` function in this library. The
    /// difference is that this coroutine is scoped, meaning that it's guaranteed to terminate
    /// before the current stack frame goes away, allowing you to reference the parent stack frame
    /// directly. This is ensured by having the parent thread join on the child thread before the
    /// scope exits.
    ///
    /// # Examples
    ///
    /// A basic scoped thread:
    ///
    /// ```
    /// coroutine::scope(|scope| {
    ///     scope.spawn(|| {
    ///         println!("Hello from a scoped coroutine!");
    ///     });
    /// });
    /// ```
    ///
    /// When writing concurrent Rust programs, you'll sometimes see a pattern like this, using
    /// [`std::thread::spawn`][spawn]:
    ///
    /// ```ignore
    /// let array = [1, 2, 3];
    /// let mut guards = vec![];
    ///
    /// for i in &array {
    ///     let guard = std::thread::spawn(move || {
    ///         println!("element: {}", i);
    ///     });
    ///
    ///     guards.push(guard);
    /// }
    ///
    /// for guard in guards {
    ///     guard.join().unwrap();
    /// }
    /// ```
    ///
    /// The basic pattern is:
    ///
    /// 1. Iterate over some collection.
    /// 2. Spin up a thread to operate on each part of the collection.
    /// 3. Join all the threads.
    ///
    /// However, this code actually gives an error:
    ///
    /// ```text
    /// error: `array` does not live long enough
    /// for i in &array {
    ///           ^~~~~
    /// in expansion of for loop expansion
    /// note: expansion site
    /// note: reference must be valid for the static lifetime...
    /// note: ...but borrowed value is only valid for the block suffix following statement 0 at ...
    ///     let array = [1, 2, 3];
    ///     let mut guards = vec![];
    ///
    ///     for i in &array {
    ///         let guard = std::thread::spawn(move || {
    ///             println!("element: {}", i);
    /// ...
    /// error: aborting due to previous error
    /// ```
    ///
    /// Because [`std::thread::spawn`][spawn] doesn't know about this scope, it requires a
    /// `'static` lifetime. One way of giving it a proper lifetime is to use an [`Arc`][arc]:
    ///
    /// [arc]: http://doc.rust-lang.org/stable/std/sync/struct.Arc.html
    ///
    /// ```
    /// use std::sync::Arc;
    ///
    /// let array = Arc::new([1, 2, 3]);
    /// let mut guards = vec![];
    ///
    /// for i in (0..array.len()) {
    ///     let a = array.clone();
    ///
    ///     let guard = std::thread::spawn(move || {
    ///         println!("element: {}", a[i]);
    ///     });
    ///
    ///     guards.push(guard);
    /// }
    ///
    /// for guard in guards {
    ///     guard.join().unwrap();
    /// }
    /// ```
    ///
    /// But this introduces unnecessary allocation, as `Arc<T>` puts its data on the heap, and we
    /// also end up dealing with reference counts. We know that we're joining the threads before
    /// our function returns, so just taking a reference _should_ be safe. Rust can't know that,
    /// though.
    ///
    /// Enter scoped coroutine. Here's our original example, using `spawn` from scope rather
    /// than from `coroutine::spawn`:
    ///
    /// ```
    /// let array = [1, 2, 3];
    ///
    /// coroutine::scope(|scope| {
    ///     for i in &array {
    ///         scope.spawn(move || {
    ///             println!("element: {}", i);
    ///         });
    ///     }
    /// });
    /// ```
    ///
    /// Much more straightforward.
    pub fn spawn<F, T>(&self, f: F) -> ScopedJoinHandle<T>
        where F: FnOnce() -> T + Send + 'a,
              T: Send + 'a
    {
        let their_packet = Arc::new(SyncCell(RefCell::new(None)));
        let my_packet = their_packet.clone();

        let join_handle = unsafe {
            spawn_unsafe(move || {
                *their_packet.0.borrow_mut() = Some(f());
            })
        };

        let deferred_handle = Rc::new(RefCell::new(JoinState::Running(join_handle)));
        let my_handle = deferred_handle.clone();

        self.defer(move || {
            let mut state = deferred_handle.borrow_mut();
            state.join();
        });

        ScopedJoinHandle {
            inner: my_handle,
            packet: my_packet,
        }
    }
}

impl<T> ScopedJoinHandle<T> {
    /// Join the scoped thread, returning the result it produced.
    pub fn join(self) -> T {
        self.inner.borrow_mut().join();
        self.packet.0.borrow_mut().take().unwrap()
    }
}

impl<'a> Drop for Scope<'a> {
    fn drop(&mut self) {
        self.drop_all()
    }
}
