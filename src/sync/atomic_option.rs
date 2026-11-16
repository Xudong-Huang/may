use crossbeam::atomic::AtomicCell;

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
// the boxed fallback must itself be lock-free, otherwise it would serialize
// through `AtomicCell`'s process-wide sharded lock just like the inline path
const _: () = assert!(AtomicCell::<Option<Box<()>>>::is_lock_free());

/// A minimal atomic `Option<T>`.
///
/// Used by the synchronization primitives as a single-slot hand-off. When
/// `Option<T>` fits `AtomicCell`'s atomic path (asserted above for the
/// crate's hot-path uses) the value is stored inline and the cell is lock-free.
/// Otherwise the value is boxed, so the cell stores a pointer and is still
/// lock-free instead of falling back to `AtomicCell`'s process-wide sharded
/// lock. The boxed path pays one allocation per stored value; it is only
/// reached by the instantiations whose value is too big for an atomic word
/// (coroutine result packets and panic payloads).
///
/// The operations carry the backing `AtomicCell`'s orderings: `store` is a
/// release store and `swap`/`take` are acq-rel read-modify-writes. They are
/// not sequentially consistent, so a two-sided handshake that must not let
/// each side miss the other (the io `wait`/`io_flag` handshake, for example)
/// pairs its operations with a `fence(SeqCst)` on both sides.
pub struct AtomicOption<T> {
    /// The inline, lock-free slot used when `Option<T>` fits an atomic word.
    cell: AtomicCell<Option<T>>,
    /// The pointer-sized fallback used otherwise; `Option<Box<T>>` is always
    /// lock-free.
    boxed: AtomicCell<Option<Box<T>>>,
}

impl<T> AtomicOption<T> {
    /// Whether the inline `AtomicCell` path applies. When this is false the
    /// value is boxed instead. It is a constant for a given `T`, so every
    /// thread agrees on which representation a cell uses.
    pub const DIRECT_ATOMIC: bool = AtomicCell::<Option<T>>::is_lock_free();

    /// Create an empty slot.
    pub const fn none() -> AtomicOption<T> {
        AtomicOption {
            cell: AtomicCell::new(None),
            boxed: AtomicCell::new(None),
        }
    }

    /// Create a slot containing `t`.
    ///
    /// Not a `const fn`: the boxed fallback allocates.
    pub fn some(t: T) -> AtomicOption<T> {
        let slot = AtomicOption::none();
        slot.store(t);
        slot
    }

    /// Store `t`, dropping any previous value.
    #[inline]
    pub fn store(&self, t: T) {
        if Self::DIRECT_ATOMIC {
            self.cell.store(Some(t));
        } else {
            self.boxed.store(Some(Box::new(t)));
        }
    }

    /// Take the value out of the slot, leaving it empty.
    #[inline]
    pub fn take(&self) -> Option<T> {
        if Self::DIRECT_ATOMIC {
            self.cell.take()
        } else {
            self.boxed.take().map(|boxed| *boxed)
        }
    }

    /// Replace the value in the slot, returning the previous value.
    #[inline]
    pub fn swap(&self, t: T) -> Option<T> {
        if Self::DIRECT_ATOMIC {
            self.cell.swap(Some(t))
        } else {
            self.boxed.swap(Some(Box::new(t))).map(|boxed| *boxed)
        }
    }

    /// Empty the slot, dropping any value.
    #[inline]
    pub fn clear(&self) {
        if Self::DIRECT_ATOMIC {
            self.cell.store(None);
        } else {
            self.boxed.store(None);
        }
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

    // a value too large for an atomic word takes the boxed fallback
    #[test]
    fn boxed_hand_off() {
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

    // the slot stays `Send + Sync` for a `Send` payload even when it is not
    // `Sync`, and the boxed fallback moves ownership without constraining `T`
    #[test]
    fn non_sync_payload() {
        assert_send_sync::<AtomicOption<Box<dyn Any + Send>>>();
        // on 64-bit the 16-byte payload takes the boxed fallback
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
        // keep the value too large for the inline atomic word on any target
        _pad: u64,
    }

    impl Drop for Tracked {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }

    // concurrent swaps must hand every value to exactly one taker and drop
    // each value exactly once
    #[test]
    fn boxed_concurrent_hand_off() {
        const THREADS: usize = 4;
        const ROUNDS: usize = 10_000;
        let total = THREADS * ROUNDS;

        const _: () = assert!(!AtomicOption::<Tracked>::DIRECT_ATOMIC);
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
                            _pad: 0,
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
