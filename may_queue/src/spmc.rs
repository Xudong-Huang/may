use crossbeam::utils::{Backoff, CachePadded};
use smallvec::SmallVec;

use crate::atomic::{AtomicPtr, AtomicUsize};

use std::cell::UnsafeCell;
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::ptr;
use std::sync::atomic::Ordering;
use std::sync::Arc;

// size for block_node
pub const BLOCK_SIZE: usize = 1 << BLOCK_SHIFT;
// block mask
pub const BLOCK_MASK: usize = BLOCK_SIZE - 1;
// block shift
pub const BLOCK_SHIFT: usize = 5;

/// A slot in a block.
struct Slot<T> {
    /// The value.
    value: UnsafeCell<MaybeUninit<T>>,
}

impl<T> Slot<T> {
    const UNINIT: Self = Self {
        value: UnsafeCell::new(MaybeUninit::uninit()),
    };
}

/// a block node contains a bunch of items stored in a array
/// this could make the malloc/free not that frequent, also
/// the array could speed up list operations
#[repr(align(32))]
struct BlockNode<T> {
    data: [Slot<T>; BLOCK_SIZE],
    used: AtomicUsize,
    next: AtomicPtr<BlockNode<T>>,
    start: AtomicUsize, // start index of the block
}

/// we don't implement the block node Drop trait
/// the queue is responsible to drop all the items
/// and would call its get() method for the dropping
impl<T> BlockNode<T> {
    /// create a new BlockNode with uninitialized data
    #[inline]
    pub fn new(index: usize) -> *mut BlockNode<T> {
        Box::into_raw(Box::new(BlockNode {
            next: AtomicPtr::new(ptr::null_mut()),
            used: AtomicUsize::new(BLOCK_SIZE),
            data: [Slot::UNINIT; BLOCK_SIZE],
            start: AtomicUsize::new(index),
        }))
    }

    /// write index with data
    #[inline]
    pub fn set(&self, index: usize, v: T) {
        unsafe {
            let data = self.data.get_unchecked(index & BLOCK_MASK);
            data.value.get().write(MaybeUninit::new(v));
        }
    }

    /// read out indexed value
    /// this would make the underlying data dropped when it get out of scope
    #[inline]
    pub fn get(&self, id: usize) -> T {
        unsafe {
            let data = self.data.get_unchecked(id);
            data.value.get().read().assume_init()
        }
    }

    /// make a range slots read
    /// if all slots read, then we can safely free the block
    #[inline]
    pub fn mark_slots_read(&self, size: usize) -> bool {
        let old = self.used.fetch_sub(size, Ordering::Relaxed);
        old == size
    }

    #[inline]
    pub fn copy_to_bulk(&self, start: usize, end: usize) -> SmallVec<[T; BLOCK_SIZE]> {
        let len = end - start;
        let start = start & BLOCK_MASK;
        (start..start + len).map(|id| self.get(id)).collect()
    }
}

/// A position in a queue.
#[derive(Debug)]
struct Position<T> {
    /// The index in the queue.
    index: AtomicUsize,

    /// The block in the linked list.
    block: AtomicPtr<BlockNode<T>>,
}

impl<T> Position<T> {
    fn new(block: *mut BlockNode<T>) -> Self {
        Position {
            index: AtomicUsize::new(0),
            block: AtomicPtr::new(block),
        }
    }
}

#[derive(Debug)]
struct BlockPtr<T>(AtomicPtr<BlockNode<T>>);

impl<T> BlockPtr<T> {
    #[inline]
    fn new(block: *mut BlockNode<T>) -> Self {
        BlockPtr(AtomicPtr::new(block))
    }

    #[inline]
    fn unpack(ptr: *mut BlockNode<T>) -> (*mut BlockNode<T>, usize) {
        let ptr = ptr as usize;
        let index = ptr & BLOCK_MASK;
        let ptr = (ptr & !BLOCK_MASK) as *mut BlockNode<T>;
        (ptr, index)
    }

    #[inline]
    fn pack(ptr: *const BlockNode<T>, index: usize) -> *mut BlockNode<T> {
        ((ptr as usize) | index) as *mut BlockNode<T>
    }
}

/// spsc queue
#[derive(Debug)]
pub struct Queue<T> {
    // ----------------------------------------
    // use for pop
    head: CachePadded<BlockPtr<T>>,

    // -----------------------------------------
    // use for push
    tail: CachePadded<Position<T>>,

    /// Indicates that dropping a `Queue<T>` may drop values of type `T`.
    _marker: PhantomData<T>,
}

unsafe impl<T: Send> Send for Queue<T> {}
unsafe impl<T: Send> Sync for Queue<T> {}

impl<T> Queue<T> {
    /// create a spsc queue
    pub fn new() -> Self {
        let init_block = BlockNode::<T>::new(0);
        Queue {
            head: BlockPtr::new(init_block).into(),
            tail: Position::new(init_block).into(),
            _marker: PhantomData,
        }
    }

    /// push a value to the back of queue
    pub fn push(&self, v: T) {
        let tail = unsafe { &mut *self.tail.block.unsync_load() };
        let push_index = unsafe { self.tail.index.unsync_load() };
        // store the data
        tail.set(push_index, v);
        // need this to make sure the data is stored before the index is updated
        std::sync::atomic::fence(Ordering::Release);

        // alloc new block node if the tail is full
        let new_index = push_index.wrapping_add(1);
        if new_index & BLOCK_MASK == 0 {
            let new_tail = BlockNode::new(new_index);
            // when other thread access next, we already Acquire the container node
            tail.next.store(new_tail, Ordering::Release);
            self.tail.block.store(new_tail, Ordering::Relaxed);
        }

        // commit the push
        self.tail.index.store(new_index, Ordering::Release);
    }

    /// pop from the queue, if it's empty return None
    pub fn pop(&self) -> Option<T> {
        let backoff = Backoff::new();
        let mut head = self.head.0.load(Ordering::Acquire);
        let mut push_index = self.tail.index.load(Ordering::Acquire);
        let mut tail_block = self.tail.block.load(Ordering::Acquire);

        loop {
            head = (head as usize & !(1 << 63)) as *mut BlockNode<T>;
            let (block, id) = BlockPtr::unpack(head);
            if block == tail_block && id >= (push_index & BLOCK_MASK) {
                return None;
            }

            let new_head = if id != BLOCK_MASK {
                BlockPtr::pack(block, id + 1)
            } else {
                (head as usize | (1 << 63)) as *mut BlockNode<T>
            };

            let block = unsafe { &mut *block };

            // commit the pop
            match self.head.0.compare_exchange_weak(
                head,
                new_head,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    let block_start = block.start.load(Ordering::Relaxed);
                    let pop_index = block_start + id;
                    if id == BLOCK_MASK {
                        push_index = self.tail.index.load(Ordering::Acquire);
                        // we need to check if there is enough data
                        if pop_index >= push_index {
                            // recover the old head, and return None
                            self.head.0.store(head, Ordering::Release);
                            return None;
                        }

                        let next = block.next.load(Ordering::Acquire);
                        self.head.0.store(next, Ordering::Release);
                    } else {
                        // we have to wait if there is enough data
                        // if no any more produce, this will be a dead loop
                        while pop_index >= self.tail.index.load(Ordering::Acquire) {
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                    }
                    // get the data
                    let v = block.get(id);

                    if block.mark_slots_read(1) {
                        // we need to free the old block
                        let _unused_block = unsafe { Box::from_raw(block) };
                    }
                    return Some(v);
                }
                Err(i) => {
                    head = i;
                    backoff.spin();
                    push_index = self.tail.index.load(Ordering::Acquire);
                    tail_block = self.tail.block.load(Ordering::Acquire);
                }
            }
        }
    }

    /// pop from the queue, if it's empty return None
    fn local_pop(&self) -> Option<T> {
        let backoff = Backoff::new();
        let mut head = self.head.0.load(Ordering::Acquire);
        // this is used for local pop, we can sure that push_index is not changed
        let push_index = unsafe { self.tail.index.unsync_load() };
        let tail_block = unsafe { self.tail.block.unsync_load() };

        loop {
            head = (head as usize & !(1 << 63)) as *mut BlockNode<T>;
            let (block, id) = BlockPtr::unpack(head);
            if block == tail_block && id >= (push_index & BLOCK_MASK) {
                return None;
            }

            let new_head = if id != BLOCK_MASK {
                BlockPtr::pack(block, id + 1)
            } else {
                (head as usize | (1 << 63)) as *mut BlockNode<T>
            };

            let block = unsafe { &mut *block };

            // commit the pop
            match self.head.0.compare_exchange_weak(
                head,
                new_head,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    let block_start = block.start.load(Ordering::Relaxed);
                    let pop_index = block_start + id;
                    if id == BLOCK_MASK {
                        // we need to check if there is enough data
                        if pop_index >= push_index {
                            // recover the old head, and return None
                            self.head.0.store(head, Ordering::Release);
                            return None;
                        }
                        let next = block.next.load(Ordering::Acquire);
                        self.head.0.store(next, Ordering::Release);
                    } else if pop_index >= push_index {
                        // pop_index never exceed push_index
                        assert_eq!(pop_index, push_index);
                        // advance the push index and this slot is ignored
                        self.tail.index.store(push_index + 1, Ordering::Relaxed);
                        if block.mark_slots_read(1) {
                            // we need to free the old block
                            let _unused_block = unsafe { Box::from_raw(block) };
                        }
                        return None;
                    }

                    // get the data
                    let v = block.get(id);

                    if block.mark_slots_read(1) {
                        // we need to free the old block
                        let _unused_block = unsafe { Box::from_raw(block) };
                    }
                    return Some(v);
                }
                Err(i) => {
                    head = i;
                    backoff.spin();
                }
            }
        }
    }

    /// pop from the queue, if it's empty return None
    pub fn bulk_pop(&self) -> SmallVec<[T; BLOCK_SIZE]> {
        self.bulk_pop_impl(0)
    }

    /// steal pop from the queue, if not enough data return None
    /// min_left is the minimum number of elements left to owner
    #[inline]
    fn bulk_pop_impl(&self, min_left: usize) -> SmallVec<[T; BLOCK_SIZE]> {
        let mut head = self.head.0.load(Ordering::Acquire);
        let mut push_index = self.tail.index.load(Ordering::Acquire);
        let mut tail_block = self.tail.block.load(Ordering::Acquire);

        loop {
            head = (head as usize & !(1 << 63)) as *mut BlockNode<T>;
            let (block, id) = BlockPtr::unpack(head);
            let push_id = push_index & BLOCK_MASK;
            // at least leave one element to the owner
            if block == tail_block && id + min_left >= push_id {
                return SmallVec::new();
            }

            let new_id = if block != tail_block {
                0
            } else {
                push_id - min_left
            };

            let new_head = if new_id == 0 {
                (head as usize | (1 << 63)) as *mut BlockNode<T>
            } else {
                BlockPtr::pack(block, new_id)
            };

            let block = unsafe { &mut *block };
            // only pop within a block
            match self
                .head
                .0
                .compare_exchange(head, new_head, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    let block_start = block.start.load(Ordering::Relaxed);
                    let pop_index = block_start + id;

                    let end;
                    if new_id == 0 {
                        push_index = self.tail.index.load(Ordering::Acquire);
                        if pop_index >= push_index {
                            // recover the old head, and return None
                            self.head.0.store(head, Ordering::Release);
                            return SmallVec::new();
                        }
                        end = std::cmp::min(block_start + BLOCK_SIZE, push_index);
                        let new_id = end & BLOCK_MASK;
                        if new_id == 0 {
                            let next = block.next.load(Ordering::Acquire);
                            self.head.0.store(next, Ordering::Release);
                        } else {
                            let new_head = BlockPtr::pack(block, new_id);
                            self.head.0.store(new_head, Ordering::Release);
                        }
                    } else {
                        end = block_start + new_id;
                        // we have to wait there is enough data, normally this would not happen
                        // except for the ABA situation
                        // if no any more data pushed, this will be a dead loop
                        while end > self.tail.index.load(Ordering::Acquire) {
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                    }

                    // get the data
                    let value = block.copy_to_bulk(pop_index, end);

                    if block.mark_slots_read(end - pop_index) {
                        // we need to free the old block
                        let _unused_block = unsafe { Box::from_raw(block) };
                    }
                    return value;
                }
                Err(i) => {
                    head = i;
                    push_index = self.tail.index.load(Ordering::Acquire);
                    tail_block = self.tail.block.load(Ordering::Acquire);
                }
            }
        }
    }

    /// get the size of queue
    ///
    /// # Safety
    ///
    /// this function is unsafe because it's possible that the block is destroyed
    /// and the rare cases that the pop index is bigger than the push index may cause
    /// return a wrong length
    pub unsafe fn len(&self) -> usize {
        let head = self.head.0.load(Ordering::Acquire);
        let head = (head as usize & !(1 << 63)) as *mut BlockNode<T>;
        let (block, id) = BlockPtr::unpack(head);
        let block = unsafe { &mut *block };
        // it's unsafe to deref the block, because it could be a destroyed one
        // we'd better use AtomicUsize index to calc the length
        // both the tail and head are just shadows of the real tail and head
        let block_start = block.start.load(Ordering::Relaxed);
        let pop_index = block_start + id;
        let push_index = self.tail.index.load(Ordering::Acquire);
        push_index.wrapping_sub(pop_index)
    }

    /// if the queue is empty
    pub fn is_empty(&self) -> bool {
        let head = self.head.0.load(Ordering::Acquire);
        let head = (head as usize & !(1 << 63)) as *mut BlockNode<T>;
        let (block, id) = BlockPtr::unpack(head);

        let push_index = self.tail.index.load(Ordering::Acquire);
        let tail_block = self.tail.block.load(Ordering::Acquire);

        block == tail_block && id == (push_index & BLOCK_MASK)
    }
}

impl<T> Default for Queue<T> {
    fn default() -> Self {
        Queue::new()
    }
}

impl<T> Drop for Queue<T> {
    fn drop(&mut self) {
        //  pop all the element to make sure the queue is empty
        while !self.bulk_pop().is_empty() {}
        let head = self.head.0.load(Ordering::Acquire);
        let (block, _id) = BlockPtr::unpack(head);
        let tail = self.tail.block.load(Ordering::Acquire);
        assert_eq!(block, tail);

        let _unused_block = unsafe { Box::from_raw(block) };
    }
}

/// Create a new local run-queue
pub fn local<T: 'static>() -> (Steal<T>, Local<T>) {
    let inner = Arc::new(Queue::new());

    let local = Local {
        inner: inner.clone(),
    };

    let remote = Steal(inner);

    (remote, local)
}

/// Producer handle. May only be used from a single thread.
pub struct Local<T: 'static> {
    inner: Arc<Queue<T>>,
}

/// Consumer handle. May be used from many threads.
pub struct Steal<T: 'static>(Arc<Queue<T>>);

impl<T> Local<T> {
    /// Returns true if the queue has entries that can be stolen.
    #[inline]
    pub fn is_stealable(&self) -> bool {
        !self.inner.is_empty()
    }

    /// Returns false if there are any entries in the queue
    ///
    /// Separate to is_stealable so that refactors of is_stealable to "protect"
    /// some tasks from stealing won't affect this
    #[inline]
    pub fn has_tasks(&self) -> bool {
        !self.inner.is_empty()
    }

    /// Pushes a task to the back of the local queue
    #[inline]
    pub fn push_back(&mut self, task: T) {
        self.inner.push(task)
    }

    /// Pops a task from the local queue.
    #[inline]
    pub fn pop(&mut self) -> Option<T> {
        self.inner.local_pop()
    }
}

impl<T> Steal<T> {
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    // #[inline]
    // pub fn len(&self) -> usize {
    //     self.0.len()
    // }

    /// Steals block of tasks from self and place them into `dst`.
    #[inline]
    pub fn steal_into(&self, dst: &mut Local<T>) -> Option<T> {
        const MIN_LEFT: usize = 2;
        let mut v = self.0.bulk_pop_impl(MIN_LEFT);
        let ret = v.pop();
        for t in v {
            dst.push_back(t);
        }
        ret
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

#[cfg(all(nightly, test))]
mod test {
    extern crate test;
    use self::test::Bencher;
    use super::*;

    use std::sync::Arc;
    use std::thread;

    use crate::test_queue::ScBlockPop;

    impl<T> ScBlockPop<T> for super::Queue<T> {
        fn block_pop(&self) -> T {
            let backoff = Backoff::new();
            loop {
                match self.pop() {
                    Some(v) => return v,
                    None => backoff.snooze(),
                }
            }
        }
    }

    #[test]
    fn queue_sanity() {
        let q = Queue::<usize>::new();
        assert!(q.is_empty());
        for i in 0..100 {
            q.push(i);
        }
        assert_eq!(unsafe { q.len() }, 100);
        println!("{q:?}");

        for i in 0..100 {
            assert_eq!(q.pop(), Some(i));
        }
        assert_eq!(q.pop(), None);
        assert!(q.is_empty());
    }

    #[bench]
    fn single_thread_test(b: &mut Bencher) {
        let q = Queue::new();
        let mut i = 0;
        b.iter(|| {
            q.push(i);
            assert_eq!(q.pop(), Some(i));
            i += 1;
        });
    }

    #[bench]
    fn multi_1p1c_test(b: &mut Bencher) {
        b.iter(|| {
            let q = Arc::new(Queue::new());
            let total_work: usize = 1_000_000;
            // create worker threads that generate mono increasing index
            let _q = q.clone();
            // in other thread the value should be still 100
            thread::spawn(move || {
                for i in 0..total_work {
                    _q.push(i);
                }
            });

            for i in 0..total_work {
                let v = q.block_pop();
                assert_eq!(i, v);
            }
        });
    }

    #[bench]
    fn multi_1p2c_test(b: &mut Bencher) {
        b.iter(|| {
            let q = Arc::new(Queue::new());
            let total_work: usize = 1_000_000;
            // create worker threads that generate mono increasing index
            // in other thread the value should be still 100
            for i in 0..total_work {
                q.push(i);
            }

            let sum = AtomicUsize::new(0);
            let threads = 20;
            thread::scope(|s| {
                (0..threads).for_each(|_| {
                    s.spawn(|| {
                        let mut total = 0;
                        for _i in 0..total_work / threads {
                            total += q.block_pop();
                        }
                        sum.fetch_add(total, Ordering::Relaxed);
                    });
                });
            });
            assert!(q.is_empty());
            assert_eq!(sum.load(Ordering::Relaxed), (0..total_work).sum());
        });
    }

    #[bench]
    fn bulk_1p2c_test(b: &mut Bencher) {
        b.iter(|| {
            let q = Arc::new(Queue::new());
            let total_work: usize = 1_000_000;
            // create worker threads that generate mono increasing index
            // in other thread the value should be still 100
            for i in 0..total_work {
                q.push(i);
            }

            let total = Arc::new(AtomicUsize::new(0));

            thread::scope(|s| {
                let threads = 20;
                for _ in 0..threads {
                    let q = q.clone();
                    let total = total.clone();
                    s.spawn(move || {
                        while !q.is_empty() {
                            let v = q.bulk_pop();
                            if !v.is_empty() {
                                total.fetch_add(v.len(), Ordering::AcqRel);
                            }
                        }
                    });
                }
            });
            assert!(q.is_empty());
            assert_eq!(total.load(Ordering::Acquire), total_work);
        });
    }
}
