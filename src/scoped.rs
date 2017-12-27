// modified from crossbeam

use std::fmt;
use std::mem;
use std::panic;
use std::thread;
use std::rc::Rc;
use std::sync::Arc;
use std::cell::RefCell;
use std::sync::atomic::Ordering;

use coroutine_impl::{spawn, Coroutine};
use join::JoinHandle;
use sync::AtomicOption;

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
where
    F: FnOnce() + Send + 'a,
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
            let res = handle.join();

            // TODO: when panic happened, the logic need to refine
            if !thread::panicking() {
                res.unwrap_or_else(|e| panic::resume_unwind(e));
            }
        }
    }
}

/// A handle to a scoped coroutine
pub struct ScopedJoinHandle<T> {
    inner: Rc<RefCell<JoinState>>,
    packet: Arc<AtomicOption<T>>,
    co: Coroutine,
}

/// Create a new `scope`, for deferred destructors.
///
/// Scopes, in particular, support scoped coroutine spawning.
///
pub fn scope<'a, F, R>(f: F) -> R
where
    F: FnOnce(&Scope<'a>) -> R,
{
    let mut scope = Scope {
        dtors: RefCell::new(None),
    };
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
            // is relinquished before running `dtor`
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
    where
        F: FnOnce() + 'a,
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
    /// directly. This is ensured by having the parent join on the child coroutine before the
    /// scope exits.
    pub fn spawn<F, T>(&self, f: F) -> ScopedJoinHandle<T>
    where
        F: FnOnce() -> T + Send + 'a,
        T: Send + 'a,
    {
        let their_packet = Arc::new(AtomicOption::none());
        let my_packet = their_packet.clone();

        let join_handle = unsafe {
            spawn_unsafe(move || {
                their_packet.swap(f(), Ordering::Relaxed);
            })
        };

        let co = join_handle.coroutine().clone();
        let deferred_handle = Rc::new(RefCell::new(JoinState::Running(join_handle)));
        let my_handle = deferred_handle.clone();

        self.defer(move || {
            let mut state = deferred_handle.borrow_mut();
            state.join();
        });

        ScopedJoinHandle {
            inner: my_handle,
            packet: my_packet,
            co: co,
        }
    }
}

impl<T> ScopedJoinHandle<T> {
    /// Join the scoped coroutine, returning the result it produced.
    pub fn join(self) -> T {
        self.inner.borrow_mut().join();
        self.packet.take(Ordering::Relaxed).unwrap()
    }

    /// Get the underlying coroutine handle.
    pub fn coroutine(&self) -> &Coroutine {
        &self.co
    }
}

impl<'a> Drop for Scope<'a> {
    fn drop(&mut self) {
        self.drop_all()
    }
}
