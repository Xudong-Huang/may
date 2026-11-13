use std::cell::Cell;
use std::marker::PhantomData;
use std::ptr;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use std::sync::Arc;

use crossbeam_utils::atomic::AtomicCell;
use crossbeam_utils::{Backoff, CachePadded};

struct Node<T> {
    next: AtomicPtr<Node<T>>,
    // The cell itself is the ownership token between the single consumer
    // (`pop`/`pop_if`) and the `Entry` handles (`with_mut_data`/`remove`),
    // which may run on different threads: a value is taken out with an atomic
    // swap, so only one of them can own it at a time. Taking the value out
    // temporarily is also how `with_mut_data` gets exclusive access to it.
    value: AtomicCell<Option<T>>,
    refs: AtomicUsize,
}
// linked bit is MSB, ref count is 2 for handle and list
const REF_INIT: usize = 0x1000_0002;
const REF_COUNT_MASK: usize = 0x0FFF_FFFF;

impl<T> Node<T> {
    fn new(v: Option<T>) -> *mut Node<T> {
        Box::into_raw(Box::new(Node {
            next: AtomicPtr::new(ptr::null_mut()),
            value: AtomicCell::new(v),
            refs: AtomicUsize::new(REF_INIT),
        }))
    }

    /// true if the value was not taken out permanently yet
    #[inline]
    fn is_linked(&self) -> bool {
        self.refs.load(Ordering::Acquire) & !REF_COUNT_MASK != 0
    }

    /// Takes the value out of the node.
    ///
    /// Returns `None` if the value is permanently gone (the node was popped or
    /// removed). A value that another thread holds transiently (a concurrent
    /// `pop`, `with_mut_data` or `remove`) is waited for and taken once it is
    /// put back.
    fn take_value(&self) -> Option<ValueGuard<'_, T>> {
        let backoff = Backoff::new();
        loop {
            if let Some(value) = self.value.take() {
                return Some(ValueGuard {
                    node: self,
                    value: Some(value),
                });
            }
            if !self.is_linked() {
                return None;
            }
            backoff.snooze();
        }
    }
}

/// Puts a value taken from a node back unless the caller commits it.
struct ValueGuard<'a, T> {
    node: &'a Node<T>,
    value: Option<T>,
}

impl<T> ValueGuard<'_, T> {
    #[inline]
    fn as_mut(&mut self) -> &mut T {
        self.value.as_mut().unwrap()
    }

    /// Keep the value instead of putting it back.
    #[inline]
    fn commit(mut self) -> T {
        self.value.take().unwrap()
    }
}

impl<T> Drop for ValueGuard<'_, T> {
    #[inline]
    fn drop(&mut self) {
        if let Some(value) = self.value.take() {
            self.node.value.store(Some(value));
        }
    }
}

pub struct Entry<T>(ptr::NonNull<Node<T>>);

// like `Mutex<T>`, the handle grants exclusive access to the value through
// `with_mut_data`, so the value only needs to be `Send` to share the handle
unsafe impl<T: Send> Sync for Entry<T> {}

impl<T> Entry<T> {
    /// get the internal data mut ref
    ///
    /// Returns `false` without calling `f` if the value has already been taken
    /// by the consumer (the node was popped) or by `remove`. This is safe to
    /// call while the consumer is popping the node: the value is moved out for
    /// the duration of `f`, so a handle can disarm an entry that the consumer
    /// may be taking at the same time.
    ///
    /// The value is put back when `f` returns or panics. `f` must not call back
    /// into the queue, it would wait for the value that is currently taken out.
    #[inline]
    pub fn with_mut_data<F>(&self, f: F) -> bool
    where
        F: FnOnce(&mut T),
    {
        // SAFETY: `self` keeps the node alive, and `take_value` hands the value
        // out to at most one caller at a time
        match unsafe { self.0.as_ref() }.take_value() {
            Some(mut guard) => {
                f(guard.as_mut());
                // the guard puts the value back
                true
            }
            None => false,
        }
    }

    /// judge if the node is still linked in the list
    #[inline]
    pub fn is_link(&self) -> bool {
        // SAFETY: `self` keeps the node alive
        unsafe { self.0.as_ref() }.is_linked()
    }

    #[inline]
    pub fn into_ptr(self) -> *mut Self {
        let ret = self.0.as_ptr() as *mut Self;
        ::std::mem::forget(self);
        ret
    }

    #[inline]
    /// # Safety
    ///
    /// Must use the ptr that from `Entry::into_ptr`
    pub unsafe fn from_ptr(ptr: *mut Self) -> Self {
        Entry(ptr::NonNull::new_unchecked(ptr as *mut Node<T>))
    }

    // remove the entry from it's list and return the contained value
    //
    // The value is claimed with an atomic swap and the node is only marked as
    // removed (its link bit is cleared); it stays linked until the consumer
    // reaches it, which retires it. The list reference is released by the
    // consumer, so a concurrent `pop` can't free the node while this side is
    // still touching it, and any number of handles may remove concurrently.
    pub fn remove(self) -> Option<T> {
        let node = unsafe { self.0.as_ref() };

        // claim the value: if it is gone, the consumer already popped the
        // node and there is nothing left to do
        let guard = node.take_value()?;

        // the newest node has no `next`; removing it would mean updating
        // the queue head, so leave it in the list (and leave its value to
        // the consumer, the guard puts it back)
        //
        // this is mainly used in the timer list, so it's rarely
        // the next is not contention for that we have wait some time already
        // leave the last node not removed also persist the queue for a while
        // that prevent frequent queue create and destroy
        if node.next.load(Ordering::Acquire).is_null() {
            return None;
        }

        // clear the link bit so `pop` retires the node instead of
        // delivering it; the list reference is released when the consumer
        // advances past it
        node.refs.fetch_and(REF_COUNT_MASK, Ordering::AcqRel);

        Some(guard.commit())
    }
}

impl<T> Drop for Entry<T> {
    // the node is released exactly when the last reference goes away: the
    // list reference is held until the consumer retires the node, so this can
    // run on any thread without freeing a node another thread is using
    fn drop(&mut self) {
        // SAFETY: this handle keeps the node alive until the count drops to 0
        let node = unsafe { self.0.as_ref() };
        if node.refs.fetch_sub(1, Ordering::AcqRel) == 1 {
            // SAFETY: the last reference is gone, no other thread can use it
            let _: Box<Node<T>> = unsafe { Box::from_raw(self.0.as_ptr()) };
        }
    }
}

unsafe impl<T: Send> Send for Entry<T> {}

/// Create a multi-producer single-consumer queue.
///
/// The returned [`Sender`] can be cloned and used from any thread. The
/// [`Receiver`] is not cloneable and not `Sync`, so the consuming operations
/// (`pop`/`pop_if`/`peek_with`) can only be used by the thread that owns the
/// receiver, or through a lock.
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let inner = Arc::new(Inner::new());
    (Sender(Arc::clone(&inner)), Receiver(inner, PhantomData))
}

/// The multi-producer handle.
///
/// `Sender` is cloneable, and `push` may be called from any thread.
pub struct Sender<T>(Arc<Inner<T>>);

impl<T> Clone for Sender<T> {
    #[inline]
    fn clone(&self) -> Self {
        Sender(Arc::clone(&self.0))
    }
}

impl<T> Sender<T> {
    /// Pushes a new value onto this queue.
    /// if the new node is head, indicate a true
    /// this is used to update the BH if it's a new head
    #[inline]
    pub fn push(&self, t: T) -> (Entry<T>, bool) {
        self.0.push(t)
    }
}

/// The single-consumer handle.
///
/// The receiver is not cloneable and not `Sync`, so there is exactly one handle
/// that can take values out of the queue, and it can only be used by one thread
/// at a time. It is `Send` and may be moved to the consuming thread.
pub struct Receiver<T>(Arc<Inner<T>>, PhantomData<Cell<()>>);

impl<T> Receiver<T> {
    /// if the queue is empty
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// call `f` with the head value
    ///
    /// Removed nodes (tombstones) are skipped, so this returns the first value
    /// that is still queued instead of `None` for a node that `remove` already
    /// claimed. The value is taken out while `f` runs, and put back afterwards.
    ///
    /// Note that `f` must not call back into this queue, it would wait for the
    /// value that is currently taken out.
    #[inline]
    pub fn peek_with<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&T) -> R,
    {
        self.0.peek_with(f)
    }

    /// Pops some data from this queue.
    #[inline]
    pub fn pop(&self) -> Option<T> {
        self.0.pop()
    }

    /// Pops the head value if the supplied predicate accepts it.
    #[inline]
    pub fn pop_if<F>(&self, f: &F) -> Option<T>
    where
        F: Fn(&T) -> bool,
    {
        self.0.pop_if(f)
    }
}

// the shared queue state behind a `Sender`/`Receiver` pair
struct Inner<T> {
    head: CachePadded<AtomicPtr<Node<T>>>,
    // only written by the consumer, but read by the producers
    tail: AtomicPtr<Node<T>>,
}

unsafe impl<T: Send> Send for Inner<T> {}
unsafe impl<T: Send> Sync for Inner<T> {}

impl<T> Inner<T> {
    /// Creates a new queue that is safe to share among multiple producers and
    /// one consumer.
    fn new() -> Inner<T> {
        let stub = Node::new(None);
        // there is no handle for the node, so it's ref should be 1 now
        // SAFETY: `stub` was just allocated
        unsafe { &*stub }.refs.store(1, Ordering::Relaxed);
        Inner {
            head: AtomicPtr::new(stub).into(),
            tail: AtomicPtr::new(stub),
        }
    }

    fn push(&self, t: T) -> (Entry<T>, bool) {
        let node = Node::new(Some(t));
        let prev = self.head.swap(node, Ordering::AcqRel);
        // SAFETY: `prev` was the queue head until the swap, and the queue keeps
        // it alive through its list reference until the consumer retires it
        unsafe { (*prev).next.store(node, Ordering::Release) };
        let tail = self.tail.load(Ordering::Relaxed);
        let is_head = std::ptr::eq(tail, prev);
        // SAFETY: `node` was just allocated, so the pointer is not null
        (Entry(unsafe { ptr::NonNull::new_unchecked(node) }), is_head)
    }

    /// if the queue is empty
    #[inline]
    fn is_empty(&self) -> bool {
        let tail = self.tail.load(Ordering::Relaxed);
        // the list is empty
        std::ptr::eq(self.head.load(Ordering::Acquire), tail)
    }

    /// call `f` with the head value
    ///
    /// Removed nodes (tombstones) are skipped, so this returns the first value
    /// that is still queued instead of `None` for a node that `remove` already
    /// claimed. The value is taken out while `f` runs, and put back afterwards.
    #[inline]
    fn peek_with<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&T) -> R,
    {
        let backoff = Backoff::new();
        let mut next;
        // spin until tail next become non-null
        loop {
            let tail = self.tail.load(Ordering::Relaxed);
            // the list is empty
            if std::ptr::eq(self.head.load(Ordering::Acquire), tail) {
                return None;
            }
            // SAFETY: the queue keeps its tail sentinel alive through the list
            // reference until the consumer, i.e. this thread, advances past it
            next = unsafe { (*tail).next.load(Ordering::Acquire) };
            if !next.is_null() {
                break;
            }
            backoff.snooze();
        }

        // the consumer is the only one that retires nodes, and it is
        // serialized by the `Receiver` being `!Sync`, so the nodes on this
        // chain stay alive until `advance` releases them
        // skip the nodes that a concurrent `remove` already claimed
        while !next.is_null() {
            // SAFETY: `next` is a live node on the queue chain
            let node = unsafe { &*next };
            match node.take_value() {
                // the guard puts the value back after the callback
                Some(mut guard) => return Some(f(guard.as_mut())),
                None => next = node.next.load(Ordering::Acquire),
            }
        }

        None
    }

    /// Mark `next` as consumed and move the tail to it, releasing the old tail.
    ///
    /// # Safety
    ///
    /// `tail` must be the current tail sentinel and `next` the node following
    /// it; only the consumer may call this.
    #[inline]
    unsafe fn advance(&self, tail: *mut Node<T>, next: *mut Node<T>) {
        // SAFETY: the caller guarantees both are live nodes of this queue;
        // `next` keeps its original provenance so it can be retired later
        let node = &*next;
        // clear the link bit so a concurrent handle stops waiting on the value
        node.refs.fetch_and(REF_COUNT_MASK, Ordering::AcqRel);
        // move the tail to next
        self.tail.store(next, Ordering::Relaxed);
        if (*tail).refs.fetch_sub(1, Ordering::AcqRel) == 1 {
            // release the node only when the ref count becomes 0
            let _: Box<Node<T>> = Box::from_raw(tail);
        }
    }

    fn pop_if<F>(&self, f: &F) -> Option<T>
    where
        F: Fn(&T) -> bool,
    {
        let backoff = Backoff::new();
        loop {
            let tail = self.tail.load(Ordering::Relaxed);
            // the list is empty
            if std::ptr::eq(self.head.load(Ordering::Acquire), tail) {
                return None;
            }

            // spin until tail next become non-null
            let mut next;
            loop {
                // SAFETY: the queue keeps its tail sentinel alive
                next = unsafe { (*tail).next.load(Ordering::Acquire) };
                if !next.is_null() {
                    break;
                }
                backoff.snooze();
            }

            // SAFETY: `next` is a live node on the queue chain
            let node = unsafe { &*next };
            let guard = match node.take_value() {
                Some(mut guard) => {
                    if !f(guard.as_mut()) {
                        // no pop, the guard puts the value back
                        return None;
                    }
                    guard
                }
                None => {
                    // the node was claimed by a concurrent `remove`,
                    // retire it and check the next one
                    // SAFETY: the consumer is advancing the tail to `next`
                    unsafe { self.advance(tail, next) };
                    continue;
                }
            };

            // SAFETY: the consumer is advancing the tail to `next`
            unsafe { self.advance(tail, next) };
            return Some(guard.commit());
        }
    }

    /// Pops some data from this queue.
    fn pop(&self) -> Option<T> {
        let backoff = Backoff::new();
        loop {
            let tail = self.tail.load(Ordering::Relaxed);

            // the list is empty
            if std::ptr::eq(self.head.load(Ordering::Acquire), tail) {
                return None;
            }

            // spin until tail next become non-null
            let mut next;
            loop {
                // SAFETY: the queue keeps its tail sentinel alive
                next = unsafe { (*tail).next.load(Ordering::Acquire) };
                if !next.is_null() {
                    break;
                }
                backoff.snooze();
            }

            // take the value: a concurrent `remove` may have claimed it,
            // in which case this node is a tombstone to retire
            // SAFETY: `next` is a live node on the queue chain
            let value = unsafe { &*next }.take_value();

            // SAFETY: the consumer is advancing the tail to `next`
            unsafe { self.advance(tail, next) };

            if let Some(guard) = value {
                return Some(guard.commit());
            }
            // the node was claimed by a concurrent `remove`, the tail has
            // advanced past it, check the next one
        }
    }
}

impl<T> Drop for Inner<T> {
    fn drop(&mut self) {
        while self.pop().is_some() {}
        // release the last node; an `Entry` handle may still reference it, in
        // which case the handle releases it when it is dropped
        let tail = self.tail.load(Ordering::Relaxed);
        if unsafe { (*tail).refs.fetch_sub(1, Ordering::AcqRel) } == 1 {
            let _: Box<Node<T>> = unsafe { Box::from_raw(tail) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel as std_channel;
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[test]
    fn test_queue() {
        let (tx, rx) = channel::<usize>();
        assert_eq!(rx.pop(), None);
        tx.push(1);
        tx.push(2);
        assert_eq!(rx.pop(), Some(1));
        assert_eq!(rx.pop(), Some(2));
        assert!(rx.is_empty());
        let a = tx.push(3);
        let b = tx.push(4);
        assert!(a.1);
        assert_eq!(a.0.remove(), Some(3));
        assert!(!b.1);
        assert_eq!(b.0.remove(), None);
        assert_eq!(rx.pop(), Some(4));
        assert!(rx.is_empty());

        tx.push(5);
        tx.push(6);
        tx.push(7);
        let co = |v: &usize| *v < 7;
        assert_eq!(rx.peek_with(|v| *v), Some(5));
        assert_eq!(rx.pop_if(&co), Some(5));
        assert_eq!(rx.pop_if(&co), Some(6));
        assert_eq!(rx.pop_if(&co), None);
        assert_eq!(rx.pop(), Some(7));
    }

    #[test]
    fn test() {
        let nthreads = 8;
        let nmsgs = 1000;
        let (tx, rx) = channel();
        assert!(rx.pop().is_none());
        let (done_tx, done_rx) = std_channel();

        for _ in 0..nthreads {
            let tx = tx.clone();
            let done_tx = done_tx.clone();
            thread::spawn(move || {
                for i in 0..nmsgs {
                    tx.push(i);
                }
                done_tx.send(()).unwrap();
            });
        }

        let mut i = 0;
        while i < nthreads * nmsgs {
            if rx.pop().is_some() {
                i += 1;
            }
        }
        drop(tx);
        drop(done_tx);
        for _ in 0..nthreads {
            done_rx.recv().unwrap();
        }
    }

    /// an `Entry` may outlive the queue: its node is kept alive by the handle
    /// reference and released only when the handle is dropped
    #[test]
    fn entry_may_outlive_the_queue() {
        let (tx, rx) = channel::<usize>();
        let (a, _) = tx.push(1);
        let (b, _) = tx.push(2);
        drop(tx);
        drop(rx);

        // draining the queue took the values away
        assert!(!a.with_mut_data(|_| unreachable!()));
        assert_eq!(a.remove(), None);
        assert_eq!(b.remove(), None);
    }

    /// touching an entry the consumer already popped must report that the
    /// value is gone instead of panicking on the taken `Option`
    #[test]
    fn with_mut_data_after_pop_returns_false() {
        let (tx, rx) = channel::<usize>();
        let (h, _) = tx.push(1);
        assert_eq!(rx.pop(), Some(1));

        let called = h.with_mut_data(|v| *v = 2);
        assert!(!called);
    }

    /// the handle mutating the value and the consumer popping the node race
    /// for the same `value`; each of them must see a consistent value.
    /// `NonZeroUsize` keeps `Option<T>` pointer-sized so this runs on
    /// `AtomicCell`'s lock-free path.
    #[test]
    fn with_mut_data_races_with_pop() {
        fn nz(v: usize) -> std::num::NonZeroUsize {
            std::num::NonZeroUsize::new(v).unwrap()
        }

        let rounds = if cfg!(miri) { 50 } else { 2000 };
        for _ in 0..rounds {
            let (tx, rx) = channel::<std::num::NonZeroUsize>();
            let (h, _) = tx.push(nz(1));
            let barrier = Arc::new(Barrier::new(2));

            let b = barrier.clone();
            let handle = thread::spawn(move || {
                b.wait();
                h.with_mut_data(|v| *v = nz(2))
            });

            barrier.wait();
            let popped = rx.pop().expect("push was not popped");
            let mutated = handle.join().unwrap();

            // the mutation is visible to the pop exactly when it won the race
            assert_eq!(popped.get(), if mutated { 2 } else { 1 });
        }
    }

    /// the consumer popping the oldest node races with `Entry::remove` on the
    /// same node; the value must be delivered exactly once
    #[test]
    fn remove_races_with_pop() {
        let rounds = if cfg!(miri) { 50 } else { 2000 };
        for _ in 0..rounds {
            let (tx, rx) = channel::<usize>();
            let (h, _) = tx.push(10usize);
            tx.push(20);
            let barrier = Arc::new(Barrier::new(2));

            let b = barrier.clone();
            let handle = thread::spawn(move || {
                b.wait();
                h.remove()
            });

            barrier.wait();
            let popped = rx.pop();
            let removed = handle.join().unwrap();

            let mut got = Vec::new();
            got.extend(popped);
            got.extend(removed);
            while let Some(v) = rx.pop() {
                got.push(v);
            }
            got.sort();
            assert_eq!(got, vec![10, 20]);
        }
    }
}
