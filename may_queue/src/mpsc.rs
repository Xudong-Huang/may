use crossbeam::utils::{Backoff, CachePadded};
use smallvec::SmallVec;

use crate::atomic::{AtomicPtr, AtomicUsize};

use std::cell::UnsafeCell;
use std::cmp;
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

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
    ready: AtomicBool,
}

impl<T> Slot<T> {
    const UNINIT: Self = Self {
        value: UnsafeCell::new(MaybeUninit::uninit()),
        ready: AtomicBool::new(false),
    };
}

/// a block node contains a bunch of items stored in a array
/// this could make the malloc/free not that frequent, also
/// the array could speed up list operations
#[repr(align(32))]
struct BlockNode<T> {
    data: [Slot<T>; BLOCK_SIZE],
    next: AtomicPtr<BlockNode<T>>,
    start: usize, // start index of the block
}

/// we don't implement the block node Drop trait
/// the queue is responsible to drop all the items
/// and would call its get() method for the dropping
impl<T> BlockNode<T> {
    /// create a new BlockNode with uninitialized data
    #[inline]
    pub fn new_box(index: usize) -> *mut BlockNode<T> {
        Box::into_raw(Box::new(BlockNode::new(index)))
    }

    /// create a new BlockNode with uninitialized data
    #[inline]
    pub fn new(index: usize) -> BlockNode<T> {
        BlockNode {
            next: AtomicPtr::new(ptr::null_mut()),
            data: [Slot::UNINIT; BLOCK_SIZE],
            start: index,
        }
    }

    /// write index with data
    #[inline]
    pub fn set(&self, index: usize, v: T) {
        unsafe {
            let data = self.data.get_unchecked(index & BLOCK_MASK);
            data.value.get().write(MaybeUninit::new(v));

            std::sync::atomic::fence(Ordering::Release);
            // mark the data ready
            data.ready.store(true, Ordering::Release);
        }
    }

    #[inline]
    pub fn get(&self, id: usize) -> T {
        let backoff = Backoff::new();
        let data = unsafe { self.data.get_unchecked(id) };
        loop {
            if data.ready.load(Ordering::Acquire) {
                return unsafe { data.value.get().read().assume_init() };
            }
            backoff.spin();
        }
    }

    #[inline]
    pub fn wait_next_block(&self) -> *mut BlockNode<T> {
        let backoff = Backoff::new();
        loop {
            let next = self.next.load(Ordering::Acquire);
            if !next.is_null() {
                return next;
            }
            backoff.spin();
        }
    }

    #[inline]
    pub fn copy_to_bulk(&self, start: usize, end: usize) -> SmallVec<[T; BLOCK_SIZE]> {
        let len = end - start;
        let start = start & BLOCK_MASK;
        SmallVec::from_iter((start..start + len).map(|i| self.get(i)))
    }
}

/// return the bulk end with in the block
#[inline]
pub fn bulk_end(start: usize, end: usize) -> usize {
    let block_end = (start + BLOCK_SIZE) & !BLOCK_MASK;
    cmp::min(end, block_end)
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
    // -----------------------------------------
    // use for push
    tail: CachePadded<BlockPtr<T>>,

    // ----------------------------------------
    // use for pop
    head: Position<T>,

    /// Indicates that dropping a `SegQueue<T>` may drop values of type `T`.
    _marker: PhantomData<T>,
}

unsafe impl<T: Send> Send for Queue<T> {}
unsafe impl<T: Send> Sync for Queue<T> {}

impl<T> Queue<T> {
    /// create a spsc queue
    pub fn new() -> Self {
        let init_block = BlockNode::new_box(0);
        let next_block = BlockNode::new_box(BLOCK_SIZE);
        unsafe { &*init_block }
            .next
            .store(next_block, Ordering::Relaxed);
        Queue {
            head: Position::new(init_block),
            tail: BlockPtr::new(init_block).into(),
            _marker: PhantomData,
        }
    }

    /// push a value to the back of queue
    pub fn push(&self, v: T) {
        let backoff = Backoff::new();
        let mut tail = self.tail.0.load(Ordering::Acquire);

        loop {
            tail = (tail as usize & !(1 << 63)) as *mut BlockNode<T>;
            let (block, id) = BlockPtr::unpack(tail);
            let block = unsafe { &*block };

            let new_tail = if id < BLOCK_MASK {
                BlockPtr::pack(block, id + 1)
            } else {
                // this is not correct, the block may be dropped
                (tail as usize | (1 << 63)) as *mut BlockNode<T>
            };

            match self.tail.0.compare_exchange_weak(
                tail,
                new_tail,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    if id == BLOCK_MASK {
                        let new_block = block.wait_next_block();
                        self.tail.0.store(new_block, Ordering::Release);
                        // install the next-next block
                        let next_block = BlockNode::new_box(block.start + BLOCK_SIZE * 2);
                        unsafe { &*new_block }
                            .next
                            .store(next_block, Ordering::Release);
                    }
                    // set the data
                    block.set(id, v);
                    return;
                }
                Err(old) => {
                    tail = old;
                    backoff.spin();
                }
            }
        }
    }

    /// pop from the queue, if it's empty return None
    fn pop(&self) -> Option<T> {
        let tail = self.tail.0.load(Ordering::Acquire);
        let (tail_block, id) = BlockPtr::unpack(tail);
        let tail_block = unsafe { &*((tail_block as usize & !(1 << 63)) as *mut BlockNode<T>) };
        let push_index = tail_block.start + id;

        // this is use for local pop, we can sure that pop_index is not changed
        let pop_index = unsafe { self.head.index.unsync_load() };
        if pop_index >= push_index {
            return None;
        }

        let id = pop_index & BLOCK_MASK;
        let head = unsafe { &mut *self.head.block.unsync_load() };

        // get the data
        let data = head.get(id);

        if id == BLOCK_MASK {
            let next_block = unsafe { head.next.unsync_load() };
            let _unused_block = unsafe { Box::from_raw(head) };
            self.head.block.store(next_block, Ordering::Relaxed);
        }

        self.head.index.store(pop_index + 1, Ordering::Relaxed);

        Some(data)
    }

    /// pop from the queue, if it's empty return None
    pub fn bulk_pop(&self) -> Option<SmallVec<[T; BLOCK_SIZE]>> {
        let tail = self.tail.0.load(Ordering::Acquire);
        let (tail_block, id) = BlockPtr::unpack(tail);
        let tail_block = unsafe { &*((tail_block as usize & !(1 << 63)) as *mut BlockNode<T>) };
        let push_index = tail_block.start + id;

        let index = unsafe { self.head.index.unsync_load() };
        if index >= push_index {
            return None;
        }

        let head = unsafe { &mut *self.head.block.unsync_load() };

        // only pop within a block
        let end = bulk_end(index, push_index);
        let value = head.copy_to_bulk(index, end);

        let new_index = end;

        // free the old block node
        if new_index & BLOCK_MASK == 0 {
            let new_head = head.next.load(Ordering::Relaxed);
            // assert!(!new_head.is_null());
            let _unused_head = unsafe { Box::from_raw(head) };
            self.head.block.store(new_head, Ordering::Relaxed);
        }

        // commit the pop
        self.head.index.store(new_index, Ordering::Relaxed);

        Some(value)
    }

    /// get the size of queue
    #[inline]
    pub fn len(&self) -> usize {
        let pop_index = self.head.index.load(Ordering::Acquire);
        let tail = self.tail.0.load(Ordering::Acquire);
        let (tail_block, id) = BlockPtr::unpack(tail);
        let tail_block = unsafe { &*((tail_block as usize & !(1 << 63)) as *mut BlockNode<T>) };
        let push_index = tail_block.start + id;
        push_index - pop_index
    }

    /// if the queue is empty
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
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
        while self.pop().is_some() {}
        let head = self.head.block.load(Ordering::Acquire);
        let tail = self.tail.0.load(Ordering::Acquire);
        let (block, _id) = BlockPtr::unpack(tail);
        assert_eq!(block, head);

        let next_block = unsafe { &*block }.next.load(Ordering::Acquire);
        assert!(!next_block.is_null());
        let _unused_block = unsafe { Box::from_raw(block) };
        let _unused_block = unsafe { Box::from_raw(next_block) };
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

    impl<T: Send> ScBlockPop<T> for super::Queue<T> {
        fn block_pop(&self) -> T {
            let backoff = crossbeam::utils::Backoff::new();
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
        assert_eq!(q.len(), 0);
        for i in 0..100 {
            q.push(i);
        }
        assert_eq!(q.len(), 100);
        println!("{q:?}");

        for i in 0..100 {
            assert_eq!(q.pop(), Some(i));
        }
        assert_eq!(q.pop(), None);
        assert_eq!(q.len(), 0);
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
    fn multi_2p1c_test(b: &mut Bencher) {
        b.iter(|| {
            let q = Arc::new(Queue::new());
            let total_work: usize = 1_000_000;
            // create worker threads that generate mono increasing index
            // in other thread the value should be still 100
            let mut total = 0;

            thread::scope(|s| {
                let threads = 20;
                for i in 0..threads {
                    let q = q.clone();
                    s.spawn(move || {
                        let len = total_work / threads;
                        let start = i * len;
                        for v in start..start + len {
                            let _v = q.push(v);
                        }
                    });
                }
                s.spawn(|| {
                    for _ in 0..total_work {
                        total += q.block_pop();
                    }
                });
            });
            assert!(q.is_empty());
            assert_eq!(total, (0..total_work).sum::<usize>());
        });
    }

    #[bench]
    fn bulk_pop_1p1c_bench(b: &mut Bencher) {
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

            let mut size = 0;
            while size < total_work {
                if let Some(v) = q.bulk_pop() {
                    for (start, i) in v.iter().enumerate() {
                        assert_eq!(*i, start + size);
                    }
                    size += v.len();
                }
            }
        });
    }

    #[bench]
    fn bulk_2p1c_test(b: &mut Bencher) {
        b.iter(|| {
            let q = Arc::new(Queue::new());
            let total_work: usize = 1_000_000;
            // create worker threads that generate mono increasing index
            // in other thread the value should be still 100
            let mut total = 0;

            thread::scope(|s| {
                let threads = 20;
                for i in 0..threads {
                    let q = q.clone();
                    s.spawn(move || {
                        let len = total_work / threads;
                        let start = i * len;
                        for v in start..start + len {
                            let _v = q.push(v);
                        }
                    });
                }
                s.spawn(|| {
                    let mut size = 0;
                    while size < total_work {
                        if let Some(v) = q.bulk_pop() {
                            size += v.len();
                            for data in v {
                                total += data;
                            }
                        }
                    }
                });
            });
            assert!(q.is_empty());
            assert_eq!(total, (0..total_work).sum::<usize>());
        });
    }
}
