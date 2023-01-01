use std::marker::PhantomData;
use std::sync::atomic::Ordering;

use crate::atomic::{AtomicPtr, AtomicUsize};
use crate::block_node::*;

use crossbeam::utils::CachePadded;
use smallvec::SmallVec;

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

/// spsc queue
#[derive(Debug)]
pub struct Queue<T> {
    // ----------------------------------------
    // use for pop
    head: CachePadded<Position<T>>,
    // -----------------------------------------
    // use for push
    tail: CachePadded<Position<T>>,

    /// Indicates that dropping a `SegQueue<T>` may drop values of type `T`.
    _marker: PhantomData<T>,
}

unsafe impl<T: Send> Send for Queue<T> {}
unsafe impl<T: Send> Sync for Queue<T> {}

impl<T> Queue<T> {
    /// create a spsc queue
    pub fn new() -> Self {
        let init_block = BlockNode::<T>::new(0);
        Queue {
            head: Position::new(init_block).into(),
            tail: Position::new(init_block).into(),
            _marker: PhantomData,
        }
    }

    /// push a value to the queue
    pub fn push(&self, v: T) {
        let tail = unsafe { &mut *self.tail.block.unsync_load() };
        let push_index = unsafe { self.tail.index.unsync_load() };
        // store the data
        tail.set(push_index, v);

        // alloc new block node if the tail is full
        let new_index = push_index.wrapping_add(1);
        if new_index & BLOCK_MASK == 0 {
            let new_tail = BlockNode::new(new_index);
            tail.next.store(new_tail, Ordering::Release);
            self.tail.block.store(new_tail, Ordering::Relaxed);
        }

        // commit the push
        self.tail.index.store(new_index, Ordering::Release);
    }

    /// pop from the queue, if it's empty return None
    pub fn pop(&self) -> Option<T> {
        let backoff = crossbeam::utils::Backoff::new();
        let mut index = self.head.index.load(Ordering::Acquire);
        // this is use for local pop, we can sure that push_index is not changed
        let push_index = self.tail.index.load(Ordering::Acquire);

        loop {
            if index == push_index {
                return None;
            }

            let new_index = index.wrapping_add(1);
            // commit the pop
            match self.head.index.compare_exchange(
                index,
                new_index,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    let head = loop {
                        let head = unsafe { &mut *self.head.block.load(Ordering::Acquire) };
                        // we have to check the block is the correct one
                        let start = head.start;
                        if start == index & !BLOCK_MASK {
                            break head;
                        } else {
                            // let used = head.used.load(Ordering::Relaxed);
                            // println!("xxxxxxxxxx, index={index}, start={start}, used={used:x}");
                            backoff.snooze();
                        }
                    };

                    // get the data
                    let v = head.get(index);

                    // free the slot
                    if head.mark_slot_read(index) {
                        let new_head = head.next.load(Ordering::Acquire);
                        assert!(!new_head.is_null());
                        // there may be other thread is using the old head, so we can't change it
                        self.head.block.store(new_head, Ordering::Release);
                        // we need to free the old head if it's get empty
                        let _unused_block = unsafe { Box::from_raw(head) };
                    }
                    return Some(v);
                }
                Err(i) => {
                    index = i;
                    continue;
                }
            }
        }
    }

    /// pop from the queue, if it's empty return None
    pub fn bulk_pop(&self) -> Option<SmallVec<[T; BLOCK_SIZE]>> {
        let backoff = crossbeam::utils::Backoff::new();
        let mut index = self.head.index.load(Ordering::Acquire);
        let push_index = self.tail.index.load(Ordering::Acquire);

        loop {
            if index == push_index {
                return None;
            }

            // let new_index = index.wrapping_add(1);
            // only pop within a block
            let end = bulk_end(index, push_index);
            // commit the pop
            match self
                .head
                .index
                .compare_exchange(index, end, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    let head = loop {
                        let head = unsafe { &mut *self.head.block.load(Ordering::Acquire) };
                        // we have to check the block is the correct one
                        let start = head.start;
                        if start == index & !BLOCK_MASK {
                            break head;
                        } else {
                            // let used = head.used.load(Ordering::Relaxed);
                            // println!("xxxxxxxxxx, index={index}, start={start}, used={used:x}");
                            backoff.snooze();
                        }
                    };

                    // get the data
                    let value = BlockNode::copy_to_bulk(head, index, end);

                    // free the slot
                    if head.mark_slots_read(index, end) {
                        let new_head = head.next.load(Ordering::Acquire);
                        assert!(!new_head.is_null());
                        // there may be other thread is using the old head, so we can't change it
                        self.head.block.store(new_head, Ordering::Release);
                        // we need to free the old head if it's get empty
                        let _unused_block = unsafe { Box::from_raw(head) };
                    }
                    return Some(value);
                }
                Err(i) => {
                    index = i;
                    continue;
                }
            }
        }
    }

    // bulk pop as much as possible
    // pub fn steal(&self, local: &mut Self) -> Option<T> {
    //     None
    // }

    /// get the size of queue
    #[inline]
    pub fn len(&self) -> usize {
        let pop_index = self.head.index.load(Ordering::Acquire);
        let push_index = self.tail.index.load(Ordering::Acquire);
        push_index.wrapping_sub(pop_index)
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
        let head = self.head.block.load(Ordering::Relaxed);
        let tail = self.tail.block.load(Ordering::Relaxed);
        assert_eq!(head, tail);

        unsafe {
            let _unused_block = Box::from_raw(head);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;
    use std::thread;

    use crate::test_queue::ScBlockPop;

    impl<T> ScBlockPop<T> for super::Queue<T> {
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
        println!("{:?}", q);

        for i in 0..100 {
            assert_eq!(q.pop(), Some(i));
        }
        assert_eq!(q.pop(), None);
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn multi_1p2c_test() {
        let q = Arc::new(Queue::new());
        let total_work: usize = 1000;
        // create worker threads that generate mono increasing index
        // in other thread the value should be still 100
        for i in 0..total_work {
            q.push(i);
        }

        thread::scope(|s| {
            let threads = 4;
            for _ in 0..threads {
                let q = q.clone();
                s.spawn(move || {
                    for _i in 0..total_work / threads {
                        let _v = q.block_pop();
                        println!("pop {_v}");
                    }
                });
            }
        });
        assert!(q.is_empty());
    }
}

#[cfg(all(nightly, test))]
mod bench {
    extern crate test;
    use self::test::Bencher;
    use super::*;

    use std::sync::Arc;
    use std::thread;

    use crate::test_queue::ScBlockPop;

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
            let total_work: usize = 1000_000;
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
            let total_work: usize = 1000_000;
            // create worker threads that generate mono increasing index
            // in other thread the value should be still 100
            for i in 0..total_work {
                q.push(i);
            }

            thread::scope(|s| {
                let threads = 20;
                for _ in 0..threads {
                    let q = q.clone();
                    s.spawn(move || {
                        for _i in 0..total_work / threads {
                            let _v = q.block_pop();
                        }
                    });
                }
            });
            assert!(q.is_empty());
        });
    }

    #[bench]
    fn bulk_1p2c_test(b: &mut Bencher) {
        b.iter(|| {
            let q = Arc::new(Queue::new());
            let total_work: usize = 1000_000;
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
                            if let Some(v) = v {
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
