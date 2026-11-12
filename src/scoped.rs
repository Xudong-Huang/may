// modified from crossbeam

use std::any::Any;
use std::fmt;
use std::io;
use std::marker::PhantomData;
use std::mem;
use std::panic;
use std::rc::Rc;
use std::sync::Arc;
use std::thread;

use parking_lot::Mutex;

use crate::coroutine_impl::{spawn_builder, Builder, Coroutine};
use crate::join::JoinHandle;
use crate::sync::AtomicOption;

/// Like `coroutine::spawn`, but without the closure bounds.
pub unsafe fn spawn_unsafe<'a, F>(f: F) -> JoinHandle<()>
where
    F: FnOnce() + Send + 'a,
{
    spawn_unsafe_builder(f, Builder::new())
}

pub unsafe fn spawn_unsafe_builder<'a, F>(f: F, builder: Builder) -> JoinHandle<()>
where
    F: FnOnce() + Send + 'a,
{
    let closure: Box<dyn FnOnce() + 'a> = Box::new(f);
    let closure: Box<dyn FnOnce() + Send> = mem::transmute(closure);
    spawn_builder(closure, builder)
}

fn cancel_error() -> Box<dyn Any + Send> {
    Box::new(generator::Error::Cancel)
}

/// A cancel is not a failure of the scoped body, so it must not be turned
/// into a panic of the enclosing scope.
fn is_cancel_panic(err: &(dyn Any + Send)) -> bool {
    matches!(
        err.downcast_ref::<generator::Error>(),
        Some(generator::Error::Cancel)
    )
}

/// A scope to spawn scoped coroutines in.
///
/// See [`scope`] for details.
pub struct Scope<'scope, 'env: 'scope> {
    // Deferred closures may borrow from the environment, which outlives the
    // scope call, and they must be `Send` because scoped coroutines can
    // register them from any worker thread.
    dtors: Mutex<Option<DtorChain<'env>>>,
    panicked: Arc<Mutex<Option<Box<dyn Any + Send>>>>,
    /// Invariance over `'scope`, so that `'scope` cannot shrink, which is
    /// necessary for soundness.
    _scope: PhantomData<&'scope mut &'scope ()>,
    /// Invariance over `'env`.
    _env: PhantomData<&'env mut &'env ()>,
}

struct DtorChain<'env> {
    dtor: Box<dyn FnOnce() + Send + 'env>,
    next: Option<Box<DtorChain<'env>>>,
}

enum JoinState {
    Running(JoinHandle<()>),
    Joined,
}

impl JoinState {
    /// Wait for the coroutine and return the panic payload if it was not
    /// already joined and it panicked.
    fn join(self) -> Option<Box<dyn Any + Send>> {
        match self {
            JoinState::Running(handle) => handle.join().err(),
            JoinState::Joined => None,
        }
    }
}

/// A handle to a scoped coroutine.
///
/// Unlike `std::thread::ScopedJoinHandle`, this handle is not `Send`: it
/// shares its join state with the scope, which joins any coroutine that was
/// not joined manually when the scope ends, so it must be joined on the
/// thread that created the scope.
pub struct ScopedJoinHandle<'scope, T> {
    inner: Arc<Mutex<JoinState>>,
    packet: Arc<AtomicOption<T>>,
    co: Coroutine,
    _scope: PhantomData<&'scope ()>,
    _not_send: PhantomData<Rc<()>>,
}

impl<'scope, 'env> Scope<'scope, 'env> {
    fn new() -> Self {
        Scope {
            dtors: Mutex::new(None),
            panicked: Arc::new(Mutex::new(None)),
            _scope: PhantomData,
            _env: PhantomData,
        }
    }

    // This method is carefully written in a transactional style, so
    // that it can be called directly and, if any dtor panics, can be
    // resumed in the unwinding this causes. By initially running the
    // method outside of any destructor, we avoid any leakage problems
    // due to @rust-lang/rust#14875.
    //
    // Unlike the old implementation, a panicking dtor does not stop the
    // remaining ones from running: every scoped coroutine has to be joined
    // before the borrowed data can go away.
    fn drop_all(&self) -> Option<Box<dyn Any + Send>> {
        let mut first_panic = None;
        loop {
            // use a separate scope to ensure that the mutex is released
            // before running `dtor`
            let dtor = {
                let mut dtors = self.dtors.lock();
                match dtors.take() {
                    Some(mut node) => {
                        *dtors = node.next.take().map(|b| *b);
                        node.dtor
                    }
                    None => return first_panic,
                }
            };
            if let Err(e) = panic::catch_unwind(panic::AssertUnwindSafe(dtor)) {
                if first_panic.is_none() {
                    first_panic = Some(e);
                }
            }
        }
    }

    /// Schedule code to be executed when exiting the scope.
    ///
    /// This is akin to having a destructor on the stack, except that it is
    /// *guaranteed* to be run.
    ///
    /// The closure must be `Send` because scoped coroutines may register
    /// deferred code from any worker thread.
    pub fn defer<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'env,
    {
        let mut dtors = self.dtors.lock();
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
    /// directly. This is ensured by having the parent join on the child coroutine before the
    /// scope exits.
    fn spawn_impl<F, T>(
        &'scope self,
        f: F,
        builder: Builder,
    ) -> io::Result<ScopedJoinHandle<'scope, T>>
    where
        F: FnOnce() -> T + Send + 'scope,
        T: Send + 'scope,
    {
        let their_packet = Arc::new(AtomicOption::none());
        let my_packet = their_packet.clone();

        let closure: Box<dyn FnOnce() + Send + 'scope> = Box::new(move || {
            their_packet.store(f());
        });
        // SAFETY: the closure may borrow data that is only valid for
        // `'scope`, but the scope joins the coroutine before `'scope` ends,
        // so the coroutine cannot outlive that data.
        let closure: Box<dyn FnOnce() + Send> = unsafe { mem::transmute(closure) };
        let join_handle = unsafe { builder.spawn(closure) }?;

        let co = join_handle.coroutine().clone();
        let deferred_handle = Arc::new(Mutex::new(JoinState::Running(join_handle)));
        let my_handle = deferred_handle.clone();
        let panicked = self.panicked.clone();

        self.defer(move || {
            // take the state out before waiting so that no lock is held
            // while the coroutine is parked
            let state = mem::replace(&mut *deferred_handle.lock(), JoinState::Joined);
            if let Some(error) = state.join() {
                if !is_cancel_panic(&*error) {
                    let mut panicked = panicked.lock();
                    if panicked.is_none() {
                        *panicked = Some(error);
                    }
                }
            }
        });

        Ok(ScopedJoinHandle {
            inner: my_handle,
            packet: my_packet,
            co,
            _scope: PhantomData,
            _not_send: PhantomData,
        })
    }

    /// Spawns a new coroutine within a scope, returning a [`ScopedJoinHandle`]
    /// for it.
    ///
    /// Unlike [`coroutine::spawn`], coroutines spawned with this method may
    /// borrow non-`'static` data from outside the scope. See [`scope`] for
    /// details.
    ///
    /// The join handle provides a [`join`] method that can be used to join the
    /// spawned coroutine. If the spawned coroutine panics, [`join`] will return
    /// an [`Err`] containing the panic payload.
    ///
    /// If the join handle is dropped, the spawned coroutine will be implicitly
    /// joined at the end of the scope. In that case, if the spawned coroutine
    /// panicked, [`scope`] will panic after all coroutines are joined.
    ///
    /// [`coroutine::spawn`]: crate::coroutine::spawn
    /// [`join`]: ScopedJoinHandle::join
    pub fn spawn<F, T>(&'scope self, f: F) -> ScopedJoinHandle<'scope, T>
    where
        F: FnOnce() -> T + Send + 'scope,
        T: Send + 'scope,
    {
        self.spawn_impl(f, Builder::new())
            .expect("failed to spawn coroutine")
    }

    /// Like [`spawn`](Self::spawn), but with a configured [`Builder`].
    ///
    /// This is the equivalent of `std::thread::Builder::spawn_scoped`, but it
    /// panics if the coroutine cannot be spawned; use
    /// [`Builder::spawn_scoped`] to get an [`io::Result`] instead.
    pub fn spawn_with_builder<F, T>(
        &'scope self,
        f: F,
        builder: Builder,
    ) -> ScopedJoinHandle<'scope, T>
    where
        F: FnOnce() -> T + Send + 'scope,
        T: Send + 'scope,
    {
        self.spawn_impl(f, builder)
            .expect("failed to spawn coroutine")
    }
}

impl Builder {
    /// Spawns a new scoped coroutine using the settings set through this
    /// `Builder`.
    ///
    /// Unlike [`Scope::spawn`], this method yields an [`io::Result`] to
    /// capture any failure to create the coroutine.
    ///
    /// This is the equivalent of `std::thread::Builder::spawn_scoped`.
    pub fn spawn_scoped<'scope, 'env, F, T>(
        self,
        scope: &'scope Scope<'scope, 'env>,
        f: F,
    ) -> io::Result<ScopedJoinHandle<'scope, T>>
    where
        F: FnOnce() -> T + Send + 'scope,
        T: Send + 'scope,
    {
        scope.spawn_impl(f, self)
    }
}

impl<T> ScopedJoinHandle<'_, T> {
    /// Waits for the associated coroutine to finish, returning the result it
    /// produced.
    ///
    /// If the associated coroutine panics, [`Err`] is returned with the panic
    /// payload.
    pub fn join(self) -> thread::Result<T> {
        let state = mem::replace(&mut *self.inner.lock(), JoinState::Joined);
        match state.join() {
            Some(e) => Err(e),
            None => self.packet.take().ok_or_else(cancel_error),
        }
    }

    /// Checks if the associated coroutine has finished.
    pub fn is_finished(&self) -> bool {
        match &*self.inner.lock() {
            JoinState::Running(handle) => handle.is_done(),
            JoinState::Joined => true,
        }
    }

    /// Get the underlying coroutine handle.
    pub fn coroutine(&self) -> &Coroutine {
        &self.co
    }
}

/// Creates a scope for spawning scoped coroutines.
///
/// The function passed to `scope` will be provided a [`Scope`] object,
/// through which scoped coroutines can be [spawned](Scope::spawn).
///
/// Unlike non-scoped coroutines, scoped coroutines can borrow non-`'static`
/// data, as the scope guarantees all coroutines will be joined at the end of
/// the scope.
///
/// All coroutines spawned within the scope that haven't been manually joined
/// will be automatically joined before this function returns.
///
/// # Panics
///
/// If any of the automatically joined coroutines panicked, this function will
/// panic after all coroutines have been joined.
///
/// If you want to handle panics from spawned coroutines, [`join`] them before
/// the end of the scope.
///
/// # Lifetimes
///
/// Scoped coroutines involve two lifetimes: `'scope` and `'env`, matching
/// `std::thread::scope`.
///
/// The `'scope` lifetime represents the lifetime of the scope itself: the time
/// during which new scoped coroutines may be spawned, and also the time during
/// which they might still be running. Once this lifetime ends, all scoped
/// coroutines are joined. It starts within `scope`, before `f` starts, and ends
/// after `f` returns and all scoped coroutines have been joined.
///
/// The `'env` lifetime represents the lifetime of whatever is borrowed by the
/// scoped coroutines. It must outlast the call to `scope`, and thus cannot be
/// smaller than `'scope`.
///
/// # Example
///
/// ```
/// use may::coroutine;
///
/// let mut a = vec![1, 2, 3];
/// let mut x = 0;
///
/// coroutine::scope(|s| {
///     s.spawn(|| {
///         println!("hello from the first scoped coroutine");
///         dbg!(&a);
///     });
///     s.spawn(|| {
///         x += a[0] + a[2];
///     });
///     println!("hello from the scope");
/// });
///
/// a.push(4);
/// assert_eq!(x, a.len());
/// ```
///
/// The closure cannot leak a [`ScopedJoinHandle`], because the `'scope`
/// lifetime is bound inside the closure:
///
/// ```compile_fail
/// let handle = may::coroutine::scope(|s| s.spawn(|| ()));
/// ```
///
/// A nested spawn may not borrow data that does not outlive the scope:
///
/// ```compile_fail
/// may::coroutine::scope(|s| {
///     s.spawn(|| {
///         let a = String::from("abcd");
///         s.spawn(|| println!("{a:?}"));
///     });
/// });
/// ```
///
/// [`join`]: ScopedJoinHandle::join
#[track_caller]
pub fn scope<'env, F, T>(f: F) -> T
where
    F: for<'scope> FnOnce(&'scope Scope<'scope, 'env>) -> T,
{
    let scope = Scope::new();

    // `Scope` intentionally has no `Drop` impl: its `'scope` parameter is the
    // lifetime of the borrow of the scope itself, and dropck would require
    // that borrow to outlive the scope's own destructor. Cleanup is done
    // explicitly instead. `f` runs under `catch_unwind` and `drop_all` cannot
    // unwind, so every path through this function joins the coroutines before
    // the borrowed data can go away.
    let result = panic::catch_unwind(panic::AssertUnwindSafe(|| f(&scope)));

    // Join all remaining spawned coroutines.
    let dtor_panic = scope.drop_all();

    match result {
        // Throw any panic from `f`.
        Err(e) => panic::resume_unwind(e),
        Ok(ret) => {
            // Throw a panic from a deferred closure or from an unjoined
            // coroutine.
            if let Some(e) = dtor_panic.or_else(|| scope.panicked.lock().take()) {
                panic::resume_unwind(e);
            }
            ret
        }
    }
}

impl fmt::Debug for Scope<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Scope {{ ... }}")
    }
}

impl<T> fmt::Debug for ScopedJoinHandle<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "ScopedJoinHandle {{ ... }}")
    }
}
