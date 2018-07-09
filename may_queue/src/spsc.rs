use std;
use std::fmt;
use std::sync::atomic::Ordering;

use block_node::*;

/// spsc queue
pub struct Queue<T> {
    // ----------------------------------------
    // keep a cache line align
    _pad0: [u8; 64],
    // use for pop
    head: *mut BlockNode<T>,
    // used to track the pop number
    pop_index: usize,
    // -----------------------------------------
    // keep a cache line align
    _pad1: [u8; 64],
    // use for push
    tail: *mut BlockNode<T>,
    // used to track the push number
    push_index: usize,
    // keep a cache line align
    _pad2: [u8; 64],
}

unsafe impl<T: Send> Send for Queue<T> {}
unsafe impl<T: Send> Sync for Queue<T> {}

impl<T> Queue<T> {
    /// create a spsc queue
    pub fn new() -> Self {
        let init_block = BlockNode::<T>::new();
        Queue {
            head: init_block,
            tail: init_block,
            push_index: 0,
            pop_index: 0,
            _pad0: [0; 64],
            _pad1: [0; 64],
            _pad2: [0; 64],
        }
    }
    /// push a value to the queue
    pub fn push(&self, v: T) {
        let me = unsafe { &mut *(self as *const _ as *mut Self) };
        let tail = unsafe { &mut *self.tail };
        // store the data
        tail.set(self.push_index, v);

        // alloc new block node if the tail is full
        let new_index = self.push_index.wrapping_add(1);
        if new_index & BLOCK_MASK == 0 {
            let new_tail = BlockNode::new();
            tail.next.store(new_tail, Ordering::Release);
            me.tail = new_tail;
        }

        // commit the push
        me.push_index = new_index;
    }

    /// pop from the queue, if it's empty return None
    pub fn pop(&self) -> Option<T> {
        let mut index = self.pop_index;
        // prevent release version wrong optimization!! use a volatile read
        let push_index = unsafe { std::ptr::read_volatile(&self.push_index) };
        if index == push_index {
            return None;
        }

        // fake self as &mut
        let me = unsafe { &mut *(self as *const _ as *mut Self) };
        let head = unsafe { &mut *self.head };

        // get the data
        let v = head.get(index);

        index = index.wrapping_add(1);
        // we need to free the old head if it's get empty
        if index & BLOCK_MASK == 0 {
            let new_head = head.next.load(Ordering::Acquire);
            assert!(!new_head.is_null());
            let _unused_head = unsafe { Box::from_raw(self.head) };
            me.head = new_head;
        }

        // commit the pop
        me.pop_index = index;

        Some(v)
    }

    /// get the size of queue
    #[inline]
    pub fn size(&self) -> usize {
        self.push_index.wrapping_sub(self.pop_index)
    }

    // here the max bulk pop should be within a block node
    pub fn bulk_pop_expect<V: Extend<T>>(&self, expect: usize, vec: &mut V) -> usize {
        let mut index = self.pop_index;
        // prevent release version wrong optimization!! use a volatile read
        let push_index = unsafe { std::ptr::read_volatile(&self.push_index) };
        if index == push_index {
            return 0;
        }

        // fake self as &mut
        let me = unsafe { &mut *(self as *const _ as *mut Self) };
        let head = unsafe { &mut *self.head };

        // only pop within a block
        let end = bulk_end(index, push_index, expect);
        let size = unsafe { head.bulk_get(index, end, vec) };

        index = end;

        // free the old block node
        if index & BLOCK_MASK == 0 {
            let new_head = head.next.load(Ordering::Acquire);
            assert!(!new_head.is_null());
            let _unused_head = unsafe { Box::from_raw(self.head) };
            me.head = new_head;
        }

        // commit the pop
        me.pop_index = index;

        size
    }

    // bulk pop as much as possible
    pub fn bulk_pop<V: Extend<T>>(&self, vec: &mut V) -> usize {
        self.bulk_pop_expect(0, vec)
    }
}

impl<T> Drop for Queue<T> {
    fn drop(&mut self) {
        //  pop all the element to make sure the queue is empty
        while self.pop().is_some() {}
        assert_eq!(self.head, self.tail);

        unsafe {
            let _unused_block = Box::from_raw(self.head);
        }
    }
}

impl<T> fmt::Debug for Queue<T> {
    #[cfg(nightly)]
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Queue<{}> {{ head: {:?}, tail: {:?}, size: {:?} }}",
            unsafe { std::intrinsics::type_name::<T>() },
            self.head,
            self.tail,
            self.push_index - self.pop_index
        )
    }

    #[cfg(not(nightly))]
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Queue<T> {{ head: {:?}, tail: {:?}, size: {:?} }}",
            self.head,
            self.tail,
            self.push_index - self.pop_index
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use block_node::BLOCK_SIZE;

    #[test]
    fn queue_sanity() {
        let q = Queue::<usize>::new();
        assert_eq!(q.size(), 0);
        for i in 0..100 {
            q.push(i);
        }
        assert_eq!(q.size(), 100);
        println!("{:?}", q);

        for i in 0..100 {
            assert_eq!(q.pop(), Some(i));
        }
        assert_eq!(q.pop(), None);
        assert_eq!(q.size(), 0);
    }

    #[test]
    fn bulk_pop_test() {
        let q = Queue::<usize>::new();
        let total_size = BLOCK_SIZE + 17;
        let mut vec = Vec::with_capacity(BLOCK_SIZE * 2);
        for i in 0..total_size {
            q.push(i);
        }
        assert_eq!(q.bulk_pop_expect(0, &mut vec), BLOCK_SIZE);
        assert_eq!(q.size(), total_size - BLOCK_SIZE);
        assert_eq!(q.bulk_pop_expect(8, &mut vec), 8);
        assert_eq!(q.bulk_pop_expect(0, &mut vec), total_size - 8 - BLOCK_SIZE);
        assert_eq!(q.size(), 0);
        println!("{:?}", q);

        for (i, item) in vec.iter().enumerate() {
            assert_eq!(*item, i);
        }
    }
}

#[cfg(all(nightly, test))]
mod bench {
    extern crate test;
    use self::test::Bencher;

    use super::*;
    use std::sync::mpsc::channel;
    use std::sync::Arc;
    use std::thread;

    use test_queue::ScBlockPop;

    #[bench]
    fn bulk_pop_1p1c_bench(b: &mut Bencher) {
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

            let mut size = 0;
            let mut vec = Vec::with_capacity(total_work);
            while size < total_work {
                size += q.bulk_pop(&mut vec);
            }

            for (i, item) in vec.iter().enumerate() {
                assert_eq!(i, *item);
            }
        });
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

    // #[bench]
    // the channel bench result show that it's 10 fold slow than our queue
    // not to mention the multi core contention
    #[allow(dead_code)]
    fn sys_stream_test(b: &mut Bencher) {
        b.iter(|| {
            let (tx, rx) = channel();
            let total_work: usize = 1000_000;
            // create worker threads that generate mono increasing index
            // in other thread the value should be still 100
            thread::spawn(move || {
                for i in 0..total_work {
                    tx.send(i).unwrap();
                }
            });

            for i in 0..total_work {
                assert_eq!(i, rx.recv().unwrap());
            }
        });
    }
}
