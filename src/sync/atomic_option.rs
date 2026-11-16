use crossbeam::atomic::AtomicCell;

#[cfg(target_pointer_width = "64")]
use rcu_cell::RcuCell;

use super::Blocker;
use crate::coroutine_impl::CoroutineImpl;
use crate::timeout_list::TimeoutHandle;

use std::sync::Arc;

/// The shared hand-off handle for one suspended coroutine.
///
/// The coroutine is taken out of it by exactly one of: an unpark, a
/// cancellation, or the scheduler timer armed for that wait. The `Arc` lets
/// the timer and the cancel machinery keep a claim on their own wait after it
/// has ended, without ever reaching a later wait registered elsewhere.
pub(crate) type WaitCo = Arc<AtomicOption<CoroutineImpl>>;

const _: () = assert!(AtomicOption::<CoroutineImpl>::DIRECT_ATOMIC);
const _: () = assert!(AtomicOption::<Arc<Blocker>>::DIRECT_ATOMIC);
const _: () = assert!(AtomicOption::<WaitCo>::DIRECT_ATOMIC);
// the timer handle slots of `park::Park` and `spsc::RecvTimeoutSource`
const _: () = assert!(AtomicOption::<TimeoutHandle<WaitCo>>::DIRECT_ATOMIC);

/// A minimal atomic `Option<T>`.
///
/// Used by the synchronization primitives as a single-slot hand-off. When
/// `Option<T>` fits `AtomicCell`'s atomic path (asserted above for the
/// crate's hot-path uses) the value is stored inline and the cell is lock-free.
/// Otherwise `AtomicCell` would serialize through its process-wide sharded
/// lock, so on 64-bit targets the value is stored in a `rcu_cell::RcuCell`
/// instead: the fallback then owns a plain `Arc<T>`, so each hand-off is
/// independent and never contends with unrelated cells. (32-bit targets keep
/// the `AtomicCell` fallback, because `rcu_cell` only supports 64-bit
/// pointers.)
///
/// The operations keep release/acquire hand-off semantics on both backends:
/// `store` is a release store, and `swap`/`take` are read-modify-writes that
/// also acquire (the `RcuCell` path follows its release compare-exchange with
/// an acquire fence). They are not sequentially consistent, so a two-sided
/// handshake that must not let each side miss the other (the io `wait`/`io_flag`
/// handshake, for example) pairs its operations with a `fence(SeqCst)` on both
/// sides.
pub struct AtomicOption<T> {
    /// The inline, lock-free slot used when `Option<T>` fits an atomic word.
    cell: AtomicCell<Option<T>>,
    /// The fallback used when `AtomicCell` would take its internal lock.
    #[cfg(target_pointer_width = "64")]
    rcu: RcuCell<T>,
}

// The slot only ever *moves* values between threads: no reference to the
// stored value is handed out (`take`/`swap` transfer ownership and
// `store`/`clear` drop the replaced value), so sharing it is safe even when
// `T` is `Send` but not `Sync`. This mirrors `AtomicCell<T>: Sync for T:
// Send`; the explicit impls are needed because the `RcuCell` fallback would
// otherwise require `T: Send + Sync`.
unsafe impl<T: Send> Send for AtomicOption<T> {}
unsafe impl<T: Send> Sync for AtomicOption<T> {}

// Like `AtomicCell`, the slot stays consistent if an operation unwinds.
impl<T> std::panic::UnwindSafe for AtomicOption<T> {}
impl<T> std::panic::RefUnwindSafe for AtomicOption<T> {}

impl<T> AtomicOption<T> {
    /// Whether the inline `AtomicCell` path applies. When this is false
    /// `AtomicCell` would use its internal lock, so the `RcuCell` backend is
    /// used instead. It is a constant for a given `T`, so every thread
    /// agrees on which backend a cell uses.
    pub const DIRECT_ATOMIC: bool = AtomicCell::<Option<T>>::is_lock_free();

    /// Create an empty slot.
    pub const fn none() -> AtomicOption<T> {
        AtomicOption {
            cell: AtomicCell::new(None),
            #[cfg(target_pointer_width = "64")]
            rcu: RcuCell::none(),
        }
    }

    /// Create a slot containing `t`.
    ///
    /// Not a `const fn`: the `RcuCell` fallback allocates the `Arc` it stores
    /// the value in.
    pub fn some(t: T) -> AtomicOption<T> {
        let slot = AtomicOption::none();
        slot.store(t);
        slot
    }

    /// Store `t`, dropping any previous value.
    #[inline]
    pub fn store(&self, t: T) {
        #[cfg(target_pointer_width = "64")]
        if !Self::DIRECT_ATOMIC {
            // `write` returns the replaced value
            drop(self.rcu.write(Arc::new(t)));
            return;
        }
        self.cell.store(Some(t));
    }

    /// Take the value out of the slot, leaving it empty.
    #[inline]
    pub fn take(&self) -> Option<T> {
        #[cfg(target_pointer_width = "64")]
        if !Self::DIRECT_ATOMIC {
            return self.rcu.take().map(rcu_into_inner);
        }
        self.cell.take()
    }

    /// Replace the value in the slot, returning the previous value.
    #[inline]
    pub fn swap(&self, t: T) -> Option<T> {
        #[cfg(target_pointer_width = "64")]
        if !Self::DIRECT_ATOMIC {
            return self.rcu.write(Arc::new(t)).map(rcu_into_inner);
        }
        self.cell.swap(Some(t))
    }

    /// Empty the slot, dropping any value.
    #[inline]
    pub fn clear(&self) {
        #[cfg(target_pointer_width = "64")]
        if !Self::DIRECT_ATOMIC {
            drop(self.rcu.take());
            return;
        }
        self.cell.store(None)
    }
}

/// Recover the owned value from the `RcuCell` backend's `Arc`.
///
/// The cell is the only owner of that `Arc`: `AtomicOption` never hands out
/// a second reference to the value, so `try_unwrap` always succeeds.
#[cfg(target_pointer_width = "64")]
#[inline]
fn rcu_into_inner<T>(arc: Arc<T>) -> T {
    match Arc::try_unwrap(arc) {
        Ok(val) => val,
        Err(_) => unreachable!("an AtomicOption value must not be shared"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::Any;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn hand_off() {
        let slot = AtomicOption::none();
        assert!(slot.take().is_none());
        slot.store(1);
        assert_eq!(slot.take(), Some(1));
        assert!(slot.take().is_none());
        assert_eq!(slot.swap(2), None);
        assert_eq!(slot.swap(3), Some(2));
        slot.clear();
        assert!(slot.take().is_none());
        assert_eq!(AtomicOption::some(4).take(), Some(4));
    }

    // a value too large for an atomic word takes the RCU fallback
    #[cfg(target_pointer_width = "64")]
    #[test]
    fn rcu_hand_off() {
        const _: () = assert!(!AtomicOption::<[u64; 4]>::DIRECT_ATOMIC);
        let slot = AtomicOption::none();
        slot.store([1; 4]);
        assert_eq!(slot.take(), Some([1; 4]));
        assert_eq!(slot.swap([2; 4]), None);
        assert_eq!(slot.swap([3; 4]), Some([2; 4]));
        slot.clear();
        assert_eq!(slot.take(), None);
        assert_eq!(AtomicOption::some([4; 4]).take(), Some([4; 4]));
    }

    // the slot is `Send + Sync` for a `Send` payload even when it is not `Sync`,
    // and the RCU fallback hands ownership over without sharing the value
    #[test]
    fn non_sync_payload() {
        assert_send_sync::<AtomicOption<Box<dyn Any + Send>>>();
        // on 64-bit the 16-byte payload takes the RCU fallback
        #[cfg(target_pointer_width = "64")]
        const _: () = assert!(!AtomicOption::<Box<dyn Any + Send>>::DIRECT_ATOMIC);
        let slot = Arc::new(AtomicOption::none());
        slot.store(Box::new(7u32) as Box<dyn Any + Send>);
        assert_eq!(*slot.take().unwrap().downcast::<u32>().unwrap(), 7);
        slot.store(Box::new(8u32) as Box<dyn Any + Send>);
        assert_eq!(*slot.take().unwrap().downcast::<u32>().unwrap(), 8);
    }

    struct Tracked {
        tag: usize,
        drops: Arc<AtomicUsize>,
    }

    impl Drop for Tracked {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }

    // concurrent swaps must hand every value to exactly one taker and drop
    // each value exactly once
    #[cfg(target_pointer_width = "64")]
    #[test]
    fn rcu_concurrent_hand_off() {
        const THREADS: usize = 4;
        const ROUNDS: usize = 10_000;
        let total = THREADS * ROUNDS;

        let drops = Arc::new(AtomicUsize::new(0));
        let slot = Arc::new(AtomicOption::none());
        let threads: Vec<_> = (0..THREADS)
            .map(|t| {
                let slot = slot.clone();
                let drops = drops.clone();
                std::thread::spawn(move || {
                    let mut taken = Vec::new();
                    for i in 0..ROUNDS {
                        let item = Tracked {
                            tag: t * ROUNDS + i,
                            drops: drops.clone(),
                        };
                        if let Some(old) = slot.swap(item) {
                            taken.push(old.tag);
                        }
                    }
                    taken
                })
            })
            .collect();

        let mut seen = vec![false; total];
        for thread in threads {
            for tag in thread.join().unwrap() {
                assert!(!seen[tag], "tag {tag} was handed out twice");
                seen[tag] = true;
            }
        }
        if let Some(last) = slot.take() {
            assert!(!seen[last.tag], "tag {} was handed out twice", last.tag);
            seen[last.tag] = true;
        }
        assert!(seen.iter().all(|&s| s), "a value was lost");
        assert_eq!(drops.load(Ordering::Relaxed), total, "a value was leaked");
    }
}
