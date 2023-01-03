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

//! Run-queue structures to support a work-stealing scheduler

use std::mem::{self, MaybeUninit};
use std::ptr;
use std::sync::atomic::Ordering::{AcqRel, Acquire, Release};
use std::sync::Arc;

#[derive(Debug)]
pub(crate) struct UnsafeCell<T>(std::cell::UnsafeCell<T>);

impl<T> UnsafeCell<T> {
    pub(crate) const fn new(data: T) -> UnsafeCell<T> {
        UnsafeCell(std::cell::UnsafeCell::new(data))
    }

    pub(crate) fn with<R>(&self, f: impl FnOnce(*const T) -> R) -> R {
        f(self.0.get())
    }

    pub(crate) fn with_mut<R>(&self, f: impl FnOnce(*mut T) -> R) -> R {
        f(self.0.get())
    }
}

// Use wider integers when possible to increase ABA resilience.
//
// See issue #5041: <https://github.com/tokio-rs/tokio/issues/5041>.

type UnsignedShort = u32;
type UnsignedLong = u64;
type AtomicUnsignedShort = crate::atomic::AtomicU32;
type AtomicUnsignedLong = std::sync::atomic::AtomicU64;

/// Producer handle. May only be used from a single thread.
pub struct Local<T: 'static> {
    inner: Arc<Inner<T>>,
}

/// Consumer handle. May be used from many threads.
pub struct Steal<T: 'static>(Arc<Inner<T>>);

pub(crate) struct Inner<T: 'static> {
    /// Concurrently updated by many threads.
    ///
    /// Contains two `UnsignedShort` values. The LSB byte is the "real" head of
    /// the queue. The `UnsignedShort` in the MSB is set by a stealer in process
    /// of stealing values. It represents the first value being stolen in the
    /// batch. The `UnsignedShort` indices are intentionally wider than strictly
    /// required for buffer indexing in order to provide ABA mitigation and make
    /// it possible to distinguish between full and empty buffers.
    ///
    /// When both `UnsignedShort` values are the same, there is no active
    /// stealer.
    ///
    /// Tracking an in-progress stealer prevents a wrapping scenario.
    head: AtomicUnsignedLong,

    /// Only updated by producer thread but read by many threads.
    tail: AtomicUnsignedShort,

    /// Elements
    buffer: Box<[UnsafeCell<MaybeUninit<T>>; LOCAL_QUEUE_CAPACITY]>,
}

unsafe impl<T> Send for Inner<T> {}
unsafe impl<T> Sync for Inner<T> {}

const LOCAL_QUEUE_CAPACITY: usize = 1024;

const MASK: usize = LOCAL_QUEUE_CAPACITY - 1;

// Constructing the fixed size array directly is very awkward. The only way to
// do it is to repeat `UnsafeCell::new(MaybeUninit::uninit())` 256 times, as
// the contents are not Copy. The trick with defining a const doesn't work for
// generic types.
fn make_fixed_size<T>(buffer: Box<[T]>) -> Box<[T; LOCAL_QUEUE_CAPACITY]> {
    assert_eq!(buffer.len(), LOCAL_QUEUE_CAPACITY);

    // safety: We check that the length is correct.
    unsafe { Box::from_raw(Box::into_raw(buffer).cast()) }
}

/// Create a new local run-queue
pub fn local<T: 'static>() -> (Steal<T>, Local<T>) {
    let mut buffer = Vec::with_capacity(LOCAL_QUEUE_CAPACITY);

    for _ in 0..LOCAL_QUEUE_CAPACITY {
        buffer.push(UnsafeCell::new(MaybeUninit::uninit()));
    }

    let inner = Arc::new(Inner {
        head: AtomicUnsignedLong::new(0),
        tail: AtomicUnsignedShort::new(0),
        buffer: make_fixed_size(buffer.into_boxed_slice()),
    });

    let local = Local {
        inner: inner.clone(),
    };

    let remote = Steal(inner);

    (remote, local)
}

impl<T> Local<T> {
    /// Returns true if the queue has entries that can be stolen.
    pub fn is_stealable(&self) -> bool {
        !self.inner.is_empty()
    }

    /// Returns false if there are any entries in the queue
    ///
    /// Separate to is_stealable so that refactors of is_stealable to "protect"
    /// some tasks from stealing won't affect this
    pub fn has_tasks(&self) -> bool {
        !self.inner.is_empty()
    }

    /// Pushes a task to the back of the local queue, skipping the LIFO slot.
    pub fn push_back(&mut self, task: T) -> Result<(), T> {
        let head = self.inner.head.load(Acquire);
        let (steal, _real) = unpack(head);

        // safety: this is the **only** thread that updates this cell.
        let tail = unsafe { self.inner.tail.unsync_load() };

        if tail.wrapping_sub(steal) >= LOCAL_QUEUE_CAPACITY as UnsignedShort {
            // Concurrently stealing, this will free up capacity, so only
            // push the task onto the inject queue
            return Err(task);
        }

        // Map the position to a slot index.
        let idx = tail as usize & MASK;

        self.inner.buffer[idx].with_mut(|ptr| {
            // Write the task to the slot
            //
            // Safety: There is only one producer and the above `if`
            // condition ensures we don't touch a cell if there is a
            // value, thus no consumer.
            unsafe {
                ptr::write((*ptr).as_mut_ptr(), task);
            }
        });

        // Make the task available. Synchronizes with a load in
        // `steal_into2`.
        self.inner.tail.store(tail.wrapping_add(1), Release);
        Ok(())
    }

    /// Pops a task from the local queue.
    pub fn pop(&mut self) -> Option<T> {
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
                Err(actual) => {
                    head = actual;
                }
            }
        };

        Some(self.inner.buffer[idx].with(|ptr| unsafe { ptr::read(ptr).assume_init() }))
    }
}

impl<T> Steal<T> {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Steals half the tasks from self and place them into `dst`.
    pub fn steal_into(&self, dst: &mut Local<T>) -> Option<T> {
        // Safety: the caller is the only thread that mutates `dst.tail` and
        // holds a mutable reference.
        let dst_tail = unsafe { dst.inner.tail.unsync_load() };

        // To the caller, `dst` may **look** empty but still have values
        // contained in the buffer. If another thread is concurrently stealing
        // from `dst` there may not be enough capacity to steal.
        let (steal, _) = unpack(dst.inner.head.load(Acquire));

        if dst_tail.wrapping_sub(steal) > LOCAL_QUEUE_CAPACITY as UnsignedShort / 2 {
            // we *could* try to steal less here, but for simplicity, we're just
            // going to abort.
            return None;
        }

        // Steal the tasks into `dst`'s buffer. This does not yet expose the
        // tasks in `dst`.
        let mut n = self.steal_into2(dst, dst_tail);

        if n == 0 {
            // No tasks were stolen
            return None;
        }

        // We are returning a task here
        n -= 1;

        let ret_pos = dst_tail.wrapping_add(n);
        let ret_idx = ret_pos as usize & MASK;

        // safety: the value was written as part of `steal_into2` and not
        // exposed to stealers, so no other thread can access it.
        let ret = dst.inner.buffer[ret_idx].with(|ptr| unsafe { ptr::read((*ptr).as_ptr()) });

        if n == 0 {
            // The `dst` queue is empty, but a single task was stolen
            return Some(ret);
        }

        // Make the stolen items available to consumers
        dst.inner.tail.store(dst_tail.wrapping_add(n), Release);

        Some(ret)
    }

    // Steal tasks from `self`, placing them into `dst`. Returns the number of
    // tasks that were stolen.
    fn steal_into2(&self, dst: &mut Local<T>, dst_tail: UnsignedShort) -> UnsignedShort {
        let mut prev_packed = self.0.head.load(Acquire);
        let mut next_packed;

        let n = loop {
            let (src_head_steal, src_head_real) = unpack(prev_packed);
            let src_tail = self.0.tail.load(Acquire);

            // If these two do not match, another thread is concurrently
            // stealing from the queue.
            if src_head_steal != src_head_real {
                return 0;
            }

            // Number of available tasks to steal
            let n = src_tail.wrapping_sub(src_head_real);
            let n = n - n / 2;

            if n == 0 {
                // No tasks available to steal
                return 0;
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
                .compare_exchange(prev_packed, next_packed, AcqRel, Acquire);

            match res {
                Ok(_) => break n,
                Err(actual) => {
                    prev_packed = actual;
                    std::hint::spin_loop();
                }
            }
        };

        assert!(
            n <= LOCAL_QUEUE_CAPACITY as UnsignedShort / 2,
            "actual = {n}"
        );

        let (first, _) = unpack(next_packed);

        // Take all the tasks
        for i in 0..n {
            // Compute the positions
            let src_pos = first.wrapping_add(i);
            let dst_pos = dst_tail.wrapping_add(i);

            // Map to slots
            let src_idx = src_pos as usize & MASK;
            let dst_idx = dst_pos as usize & MASK;

            // Read the task
            //
            // safety: We acquired the task with the atomic exchange above.
            let task = self.0.buffer[src_idx].with(|ptr| unsafe { ptr::read((*ptr).as_ptr()) });

            // Write the task to the new slot
            //
            // safety: `dst` queue is empty and we are the only producer to
            // this queue.
            dst.inner.buffer[dst_idx]
                .with_mut(|ptr| unsafe { ptr::write((*ptr).as_mut_ptr(), task) });
        }

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
                Ok(_) => return n,
                Err(actual) => {
                    let (actual_steal, actual_real) = unpack(actual);

                    assert_ne!(actual_steal, actual_real);

                    prev_packed = actual;
                    std::hint::spin_loop();
                }
            }
        }
    }

    pub fn len(&self) -> usize {
        self.0.len() as _
    }
}

impl<T> Clone for Steal<T> {
    fn clone(&self) -> Steal<T> {
        Steal(self.0.clone())
    }
}

impl<T> Drop for Local<T> {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            assert!(self.pop().is_none(), "queue not empty");
        }
    }
}

impl<T> Inner<T> {
    fn len(&self) -> UnsignedShort {
        let (_, head) = unpack(self.head.load(Acquire));
        let tail = self.tail.load(Acquire);

        tail.wrapping_sub(head)
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Split the head value into the real head and the index a stealer is working
/// on.
fn unpack(n: UnsignedLong) -> (UnsignedShort, UnsignedShort) {
    let real = n & UnsignedShort::MAX as UnsignedLong;
    let steal = n >> (mem::size_of::<UnsignedShort>() * 8);

    (steal as UnsignedShort, real as UnsignedShort)
}

/// Join the two head values
fn pack(steal: UnsignedShort, real: UnsignedShort) -> UnsignedLong {
    (real as UnsignedLong) | ((steal as UnsignedLong) << (mem::size_of::<UnsignedShort>() * 8))
}

#[test]
fn test_local_queue_capacity() {
    assert!(LOCAL_QUEUE_CAPACITY - 1 <= u16::MAX as usize);
}
