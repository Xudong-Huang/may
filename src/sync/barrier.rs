use crate::sync::{Condvar, Mutex};

use std::fmt;

/// A barrier enables multiple threads to synchronize the beginning
/// of some computation.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
/// use may::sync::Barrier;
/// use std::thread;
///
/// let n = 10;
/// let mut handles = Vec::with_capacity(n);
/// let barrier = Arc::new(Barrier::new(n));
/// for _ in 0..n {
///     let c = Arc::clone(&barrier);
///     // The same messages will be printed together.
///     // You will NOT see any interleaving.
///     handles.push(thread::spawn(move || {
///         println!("before wait");
///         c.wait();
///         println!("after wait");
///     }));
/// }
/// // Wait for other threads to finish.
/// for handle in handles {
///     handle.join().unwrap();
/// }
/// ```
pub struct Barrier {
    lock: Mutex<BarrierState>,
    cvar: Condvar,
    num_threads: usize,
}

// The inner state of a double barrier
struct BarrierState {
    count: usize,
    generation_id: usize,
}

/// A `BarrierWaitResult` is returned by [`Barrier::wait()`] when all threads
/// in the [`Barrier`] have rendezvoused.
///
/// # Examples
///
/// ```
/// use may::sync::Barrier;
///
/// let barrier = Barrier::new(1);
/// let barrier_wait_result = barrier.wait();
/// ```
pub struct BarrierWaitResult(bool);

impl fmt::Debug for Barrier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Barrier").finish_non_exhaustive()
    }
}

impl Barrier {
    /// Creates a new barrier that can block a given number of threads.
    ///
    /// A barrier will block `n`-1 threads which call [`wait()`] and then wake
    /// up all threads at once when the `n`th thread calls [`wait()`].
    ///
    /// [`wait()`]: Barrier::wait
    ///
    /// # Examples
    ///
    /// ```
    /// use may::sync::Barrier;
    ///
    /// let barrier = Barrier::new(10);
    /// ```
    #[must_use]
    #[inline]
    pub fn new(n: usize) -> Barrier {
        Barrier {
            lock: Mutex::new(BarrierState {
                count: 0,
                generation_id: 0,
            }),
            cvar: Condvar::new(),
            num_threads: n,
        }
    }

    /// Blocks the current thread until all threads have rendezvoused here.
    ///
    /// Barriers are re-usable after all threads have rendezvoused once, and can
    /// be used continuously.
    ///
    /// A single (arbitrary) thread will receive a [`BarrierWaitResult`] that
    /// returns `true` from [`BarrierWaitResult::is_leader()`] when returning
    /// from this function, and all other threads will receive a result that
    /// will return `false` from [`BarrierWaitResult::is_leader()`].
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::Arc;
    /// use may::sync::Barrier;
    /// use std::thread;
    ///
    /// let n = 10;
    /// let mut handles = Vec::with_capacity(n);
    /// let barrier = Arc::new(Barrier::new(n));
    /// for _ in 0..n {
    ///     let c = Arc::clone(&barrier);
    ///     // The same messages will be printed together.
    ///     // You will NOT see any interleaving.
    ///     handles.push(thread::spawn(move || {
    ///         println!("before wait");
    ///         c.wait();
    ///         println!("after wait");
    ///     }));
    /// }
    /// // Wait for other threads to finish.
    /// for handle in handles {
    ///     handle.join().unwrap();
    /// }
    /// ```
    pub fn wait(&self) -> BarrierWaitResult {
        let mut lock = self.lock.lock().unwrap();
        let local_gen = lock.generation_id;
        lock.count += 1;
        if lock.count < self.num_threads {
            let _guard = self
                .cvar
                .wait_while(lock, |state| local_gen == state.generation_id)
                .unwrap();
            BarrierWaitResult(false)
        } else {
            lock.count = 0;
            lock.generation_id = lock.generation_id.wrapping_add(1);
            self.cvar.notify_all();
            BarrierWaitResult(true)
        }
    }
}

impl BarrierWaitResult {
    /// Returns `true` if this thread is the "leader thread" for the call to
    /// [`Barrier::wait()`].
    ///
    /// Only one thread will have `true` returned from their result, all other
    /// threads will have `false` returned.
    ///
    /// # Examples
    ///
    /// ```
    /// use may::sync::Barrier;
    ///
    /// let barrier = Barrier::new(1);
    /// let barrier_wait_result = barrier.wait();
    /// println!("{:?}", barrier_wait_result.is_leader());
    /// ```
    #[must_use]
    pub fn is_leader(&self) -> bool {
        self.0
    }
}

#[test]
fn test_barrier() {
    use crate::sync::mpsc::channel;
    use std::sync::mpsc::TryRecvError;
    use std::sync::Arc;

    const N: usize = 10;

    let barrier = Arc::new(Barrier::new(N));
    let (tx, rx) = channel();

    for _ in 0..N - 1 {
        let c = barrier.clone();
        let tx = tx.clone();
        go!(move || {
            tx.send(c.wait().is_leader()).unwrap();
        });
    }

    // At this point, all spawned threads should be blocked,
    // so we shouldn't get anything from the port
    assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));

    let mut leader_found = barrier.wait().is_leader();

    // Now, the barrier is cleared and we should get data.
    for _ in 0..N - 1 {
        if rx.recv().unwrap() {
            assert!(!leader_found);
            leader_found = true;
        }
    }
    assert!(leader_found);
}
