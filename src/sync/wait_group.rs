use crate::sync::{Condvar, Mutex};

use std::fmt;
use std::sync::Arc;

/// Enables threads to synchronize the beginning or end of some computation.
///
/// # Wait groups vs barriers
///
/// `WaitGroup` is very similar to [`Barrier`], but there are a few differences:
///
/// * [`Barrier`] needs to know the number of threads at construction, while `WaitGroup` is cloned to
///   register more threads.
///
/// * A [`Barrier`] can be reused even after all threads have synchronized, while a `WaitGroup`
///   synchronizes threads only once.
///
/// * All threads wait for others to reach the [`Barrier`]. With `WaitGroup`, each thread can choose
///   to either wait for other threads or to continue without blocking.
///
/// # Examples
///
/// ```
/// use may::sync::WaitGroup;
/// use std::thread;
///
/// // Create a new wait group.
/// let wg = WaitGroup::new();
///
/// for _ in 0..4 {
///     // Create another reference to the wait group.
///     let wg = wg.clone();
///
///     thread::spawn(move || {
///         // Do some work.
///
///         // Drop the reference to the wait group.
///         drop(wg);
///     });
/// }
///
/// // Block until all threads have finished their work.
/// wg.wait();
/// # if cfg!(miri) { std::thread::sleep(std::time::Duration::from_millis(500)); } // wait for background threads closed: https://github.com/rust-lang/miri/issues/1371
/// ```
///
/// [`Barrier`]: std::sync::Barrier
pub struct WaitGroup {
    inner: Arc<Inner>,
}

/// Inner state of a `WaitGroup`.
struct Inner {
    cvar: Condvar,
    count: Mutex<usize>,
}

impl Default for WaitGroup {
    fn default() -> Self {
        Self {
            inner: Arc::new(Inner {
                cvar: Condvar::new(),
                count: Mutex::new(1),
            }),
        }
    }
}

impl WaitGroup {
    /// Creates a new wait group and returns the single reference to it.
    ///
    /// # Examples
    ///
    /// ```
    /// use may::sync::WaitGroup;
    ///
    /// let wg = WaitGroup::new();
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// Drops this reference and waits until all other references are dropped.
    ///
    /// # Examples
    ///
    /// ```
    /// use may::sync::WaitGroup;
    /// use std::thread;
    ///
    /// let wg = WaitGroup::new();
    ///
    /// # let t =
    /// thread::spawn({
    ///     let wg = wg.clone();
    ///     move || {
    ///         // Block until both threads have reached `wait()`.
    ///         wg.wait();
    ///     }
    /// });
    ///
    /// // Block until both threads have reached `wait()`.
    /// wg.wait();
    /// # t.join().unwrap(); // join thread to avoid https://github.com/rust-lang/miri/issues/1371
    /// ```
    pub fn wait(self) {
        if *self.inner.count.lock().unwrap() == 1 {
            return;
        }

        let inner = self.inner.clone();
        drop(self);

        let mut count = inner.count.lock().unwrap();
        while *count > 0 {
            count = inner.cvar.wait(count).unwrap();
        }
    }
}

impl Drop for WaitGroup {
    fn drop(&mut self) {
        let mut count = self.inner.count.lock().unwrap();
        *count -= 1;

        if *count == 0 {
            self.inner.cvar.notify_all();
        }
    }
}

impl Clone for WaitGroup {
    fn clone(&self) -> Self {
        let mut count = self.inner.count.lock().unwrap();
        *count += 1;

        Self {
            inner: self.inner.clone(),
        }
    }
}

impl fmt::Debug for WaitGroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count: &usize = &self.inner.count.lock().unwrap();
        f.debug_struct("WaitGroup").field("count", count).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::WaitGroup;
    use crate::sync::mpsc;

    use std::time::Duration;

    const THREADS: usize = 10;

    #[test]
    fn wait() {
        let wg = WaitGroup::new();
        let (tx, rx) = mpsc::channel();

        for _ in 0..THREADS {
            let wg = wg.clone();
            let tx = tx.clone();

            go!(move || {
                wg.wait();
                tx.send(()).unwrap();
            });
        }

        crate::coroutine::sleep(Duration::from_millis(100));

        // At this point, all spawned threads should be blocked, so we shouldn't get anything from the
        // channel.
        assert!(rx.try_recv().is_err());

        wg.wait();

        // Now, the wait group is cleared and we should receive messages.
        for _ in 0..THREADS {
            rx.recv().unwrap();
        }
    }

    #[test]
    fn wait_and_drop() {
        let wg = WaitGroup::new();
        let wg2 = WaitGroup::new();
        let (tx, rx) = mpsc::channel();

        for _ in 0..THREADS {
            let wg = wg.clone();
            let wg2 = wg2.clone();
            let tx = tx.clone();

            go!(move || {
                wg2.wait();
                tx.send(()).unwrap();
                drop(wg);
            });
        }

        // At this point, no thread has gotten past `wg2.wait()`, so we shouldn't get anything from the
        // channel.
        assert!(rx.try_recv().is_err());
        drop(wg2);

        wg.wait();

        // Now, the wait group is cleared and we should receive messages.
        for _ in 0..THREADS {
            rx.try_recv().unwrap();
        }
    }
}
