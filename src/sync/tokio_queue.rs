//! Work-stealing queue from the Tokio project.
//!
//! This file is nearly verbatim from the tokio project with only minor
//! modifications and additions. The only noteworthy modification is the
//! imposition of a limit on the number of tasks to be stolen, which was done to
//! match the behavior of `crossbeam-dequeue`.
//!
//! Copyright (c) 2022 Tokio Contributors.
//!
//! Permission is hereby granted, free of charge, to any
//! person obtaining a copy of this software and associated
//! documentation files (the "Software"), to deal in the
//! Software without restriction, including without
//! limitation the rights to use, copy, modify, merge,
//! publish, distribute, sublicense, and/or sell copies of
//! the Software, and to permit persons to whom the Software
//! is furnished to do so, subject to the following
//! conditions:
//!
//! The above copyright notice and this permission notice
//! shall be included in all copies or substantial portions
//! of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
//! ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
//! TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
//! PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
//! SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
//! CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
//! OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
//! IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
//! DEALINGS IN THE SOFTWARE.

use std::cell::UnsafeCell;
use std::fmt;
use std::mem::MaybeUninit;
use std::ops::Deref;
use std::sync::{
    atomic::AtomicU32,
    atomic::Ordering::{AcqRel, Acquire, Relaxed, Release},
    Arc,
};

/// Producer handle. May only be used from a single thread.
pub struct Local<T: 'static> {
    inner: Arc<Inner<T>>,
}

/// Consumer handle. May be used from many threads.
pub struct Steal<T: 'static>(Arc<Inner<T>>);

pub struct Inner<T: 'static> {
    /// Concurrently updated by many threads.
    ///
    /// Contains two `u16` values. The LSB byte is the "real" head of the queue.
    /// The `u16` in the MSB is set by a stealer in process of stealing values.
    /// It represents the first value being stolen in the batch. `u16` is used
    /// in order to distinguish between `head == tail` and `head == tail -
    /// capacity`.
    ///
    /// When both `u16` values are the same, there is no active stealer.
    ///
    /// Tracking an in-progress stealer prevents a wrapping scenario.
    head: AtomicU32,

    /// Only updated by producer thread but read by many threads.
    tail: AtomicU16,

    /// Tasks.
    buffer: Box<[UnsafeCell<MaybeUninit<T>>; LOCAL_QUEUE_CAPACITY]>,
}

impl<T> Drop for Inner<T> {
    fn drop(&mut self) {
        let head = unpack(self.head.load(Relaxed)).0;
        let tail = self.tail.load(Relaxed);

        let count = tail.wrapping_sub(head);

        for offset in 0..count {
            let idx = head.wrapping_add(offset) as usize & MASK;
            drop(unsafe { self.buffer[idx].get().read().assume_init() });
        }
    }
}

const LOCAL_QUEUE_CAPACITY: usize = 256;
const MASK: usize = LOCAL_QUEUE_CAPACITY - 1;

/// Limit the number of tasks to be stolen in order to match the behavior of
/// `crossbeam-dequeue`. NOTE: this does not exist in the original tokio queue.
const MAX_BATCH_SIZE: u16 = 32;

/// Error returned when stealing is unsuccessful.
#[derive(Debug, Clone, PartialEq)]
pub enum StealError {
    /// The source queue is empty.
    Empty,
    /// Another concurrent stealing operation is ongoing.
    Busy,
}

// Constructing the fixed size array directly is very awkward. The only way to
// do it is to repeat `UnsafeCell::new(MaybeUninit::uninit())` 256 times, as
// the contents are not Copy. The trick with defining a const doesn't work for
// generic types.
fn make_fixed_size<T>(buffer: Box<[T]>) -> Box<[T; LOCAL_QUEUE_CAPACITY]> {
    assert_eq!(buffer.len(), LOCAL_QUEUE_CAPACITY);

    // safety: We check that the length is correct.
    unsafe { Box::from_raw(Box::into_raw(buffer).cast()) }
}

impl<T> Default for Local<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Local<T> {
    /// Creates a new queue and returns a `Local` handle.
    pub fn new() -> Self {
        let mut buffer = Vec::with_capacity(LOCAL_QUEUE_CAPACITY);

        for _ in 0..LOCAL_QUEUE_CAPACITY {
            buffer.push(UnsafeCell::new(MaybeUninit::uninit()));
        }

        let inner = Arc::new(Inner {
            head: AtomicU32::new(0),
            tail: AtomicU16::new(0),
            buffer: make_fixed_size(buffer.into_boxed_slice()),
        });

        Local { inner }
    }

    /// Creates a new `Steal` handle associated to this `Local` handle.
    pub fn stealer(&self) -> Steal<T> {
        Steal(self.inner.clone())
    }

    /// Pushes a task to the back of the local queue, skipping the LIFO slot.
    pub fn push_back(&self, task: T) -> Result<(), T> {
        let head = self.inner.head.load(Acquire);
        let steal = unpack(head).0;

        // safety: this is the **only** thread that updates this cell.
        let tail = unsafe { self.inner.tail.unsync_load() };

        if tail.wrapping_sub(steal) >= LOCAL_QUEUE_CAPACITY as u16 {
            return Err(task);
        }

        // Map the position to a slot index.
        let idx = tail as usize & MASK;
        unsafe { self.inner.buffer[idx].get().write(MaybeUninit::new(task)) };

        // Make the task available. Synchronizes with a load in
        // `steal_into2`.
        self.inner.tail.store(tail.wrapping_add(1), Release);

        Ok(())
    }

    /// Pops a task from the local queue.
    pub fn pop(&self) -> Option<T> {
        let mut head = self.inner.head.load(Acquire);

        let idx = loop {
            let (steal, real) = unpack(head);

            // safety: this is the **only** thread that updates this cell.
            let tail = unsafe { self.inner.tail.unsync_load() };

            if real == tail {
                // queue is empty
                return None;
            }

            let next_real = real.wrapping_add(1);

            // If `steal == real` there are no concurrent stealers. Both `steal`
            // and `real` are updated.
            let next = if steal == real {
                pack(next_real, next_real)
            } else {
                assert_ne!(steal, next_real);
                pack(steal, next_real)
            };

            // Attempt to claim a task.
            let res = self
                .inner
                .head
                .compare_exchange(head, next, AcqRel, Acquire);

            match res {
                Ok(_) => break real as usize & MASK,
                Err(actual) => head = actual,
            }
        };

        Some(unsafe { self.inner.buffer[idx].get().read().assume_init() })
    }
}

unsafe impl<T> Send for Local<T> {}

impl<T> Steal<T> {
    /// Steals half the tasks from self and place them into `dst`.
    pub fn steal_into(&self, dst: &Local<T>) -> Result<T, StealError> {
        // Safety: the caller is the only thread that mutates `dst.tail` and
        // holds a mutable reference.
        let dst_tail = unsafe { dst.inner.tail.unsync_load() };

        // To the caller, `dst` may **look** empty but still have values
        // contained in the buffer. If another thread is concurrently stealing
        // from `dst` there may not be enough capacity to steal.
        let (steal, _) = unpack(dst.inner.head.load(Acquire));

        let dest_free_capacity = dst_tail.wrapping_sub(steal);

        // Steal the tasks into `dst`'s buffer. This does not yet expose the
        // tasks in `dst`. NOTE: the original tokio queue behavior has been
        // modified to impose a limit on the maximum number of tasks to steal.
        let (ret, mut n) =
            self.steal_into2(dst, dst_tail, (dest_free_capacity + 1).min(MAX_BATCH_SIZE))?;

        // We are returning a task here
        n -= 1;

        // Make the stolen tasks available to consumers
        dst.inner.tail.store(dst_tail.wrapping_add(n), Release);

        Ok(ret)
    }

    // Steal tasks from `self`, placing them into `dst`. Returns the number of
    // tasks that were stolen.
    fn steal_into2(
        &self,
        dst: &Local<T>,
        dst_tail: u16,
        max_tasks: u16,
    ) -> Result<(T, u16), StealError> {
        let mut prev_packed = self.0.head.load(Acquire);
        let mut next_packed;

        let n = loop {
            let (src_head_steal, src_head_real) = unpack(prev_packed);
            let src_tail = self.0.tail.load(Acquire);

            // If these two do not match, another thread is concurrently
            // stealing from the queue.
            if src_head_steal != src_head_real {
                return Err(StealError::Busy);
            }

            // Number of available tasks to steal
            let n = src_tail.wrapping_sub(src_head_real);

            let n = (n - n / 2).min(max_tasks);

            if n == 0 {
                // No tasks available to steal
                return Err(StealError::Empty);
            }

            // Update the real head index to acquire the tasks.
            let steal_to = src_head_real.wrapping_add(n);
            assert_ne!(src_head_steal, steal_to);
            next_packed = pack(src_head_steal, steal_to);

            // Claim all those tasks. This is done by incrementing the "real"
            // head but not the steal. By doing this, no other thread is able to
            // steal from this queue until the current thread completes.
            let res = self
                .0
                .head
                .compare_exchange(prev_packed, next_packed, Acquire, Acquire);

            match res {
                Ok(_) => break n,
                Err(actual) => prev_packed = actual,
            }
        };

        assert!(n <= LOCAL_QUEUE_CAPACITY as u16 / 2, "actual = {}", n);

        let (first, _) = unpack(next_packed);

        // Move all the tasks but the last one
        for i in 0..(n - 1) {
            // Compute the positions
            let src_pos = first.wrapping_add(i);
            let dst_pos = dst_tail.wrapping_add(i);

            // Map to slots
            let src_idx = src_pos as usize & MASK;
            let dst_idx = dst_pos as usize & MASK;

            // Read the task
            //
            // safety: We acquired the task with the atomic exchange above.
            let task = unsafe { self.0.buffer[src_idx].get().read().assume_init() };

            // Write the task to the new slot
            //
            // safety: `dst` queue is empty and we are the only producer to
            // this queue.
            unsafe {
                dst.inner.buffer[dst_idx]
                    .get()
                    .write(MaybeUninit::new(task))
            };
        }

        // Take the last task
        let src_idx = first.wrapping_add(n - 1) as usize & MASK;
        let ret = unsafe { self.0.buffer[src_idx].get().read().assume_init() };

        let mut prev_packed = next_packed;

        // Update `src_head_steal` to match `src_head_real` signalling that the
        // stealing routine is complete.
        loop {
            let head = unpack(prev_packed).1;
            next_packed = pack(head, head);

            let res = self
                .0
                .head
                .compare_exchange(prev_packed, next_packed, AcqRel, Acquire);

            match res {
                Ok(_) => return Ok((ret, n)),
                Err(actual) => {
                    let (actual_steal, actual_real) = unpack(actual);

                    assert_ne!(actual_steal, actual_real);

                    prev_packed = actual;
                }
            }
        }
    }
}

unsafe impl<T> Send for Steal<T> {}
unsafe impl<T> Sync for Steal<T> {}

impl<T> Clone for Steal<T> {
    fn clone(&self) -> Steal<T> {
        Steal(self.0.clone())
    }
}

/// Split the head value into the real head and the index a stealer is working
/// on.
fn unpack(n: u32) -> (u16, u16) {
    let real = n & u16::MAX as u32;
    let steal = n >> 16;

    (steal as u16, real as u16)
}

/// Join the two head values
fn pack(steal: u16, real: u16) -> u32 {
    (real as u32) | ((steal as u32) << 16)
}

#[test]
fn test_local_queue_capacity() {
    assert!(LOCAL_QUEUE_CAPACITY - 1 <= u8::MAX as usize);
}

/// `AtomicU16` providing an additional `load_unsync` function.
pub(crate) struct AtomicU16 {
    inner: UnsafeCell<std::sync::atomic::AtomicU16>,
}

unsafe impl Send for AtomicU16 {}
unsafe impl Sync for AtomicU16 {}

impl AtomicU16 {
    pub(crate) const fn new(val: u16) -> AtomicU16 {
        let inner = UnsafeCell::new(std::sync::atomic::AtomicU16::new(val));
        AtomicU16 { inner }
    }

    /// Performs an unsynchronized load.
    ///
    /// # Safety
    ///
    /// All mutations must have happened before the unsynchronized load.
    /// Additionally, there must be no concurrent mutations.
    pub(crate) unsafe fn unsync_load(&self) -> u16 {
        *(*self.inner.get()).get_mut()
    }
}

impl Deref for AtomicU16 {
    type Target = std::sync::atomic::AtomicU16;

    fn deref(&self) -> &Self::Target {
        // safety: it is always safe to access `&self` fns on the inner value as
        // we never perform unsafe mutations.
        unsafe { &*self.inner.get() }
    }
}

impl fmt::Debug for AtomicU16 {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.deref().fmt(fmt)
    }
}
