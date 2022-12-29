use std::cell::UnsafeCell;
use std::cmp;
use std::mem::MaybeUninit;
use std::ptr;
use std::sync::atomic::AtomicPtr;

// size for block_node
pub const BLOCK_SIZE: usize = 1 << BLOCK_SHIFT;
// block mask
pub const BLOCK_MASK: usize = BLOCK_SIZE - 1;
// block shift
pub const BLOCK_SHIFT: usize = 8;

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
pub struct BlockNode<T> {
    pub next: AtomicPtr<BlockNode<T>>,
    // we use RawVec to manage the memory
    // use an array would have it's own drop semantics which is not desired
    data: [Slot<T>; BLOCK_SIZE],
}

/// we don't implement the block node Drop trait
/// the queue is responsible to drop all the items
/// and would call its get() method for the dropping
impl<T> BlockNode<T> {
    /// create a new BlockNode with uninitialized data
    pub fn new() -> *mut BlockNode<T> {
        Box::into_raw(Box::new(BlockNode {
            next: AtomicPtr::new(ptr::null_mut()),
            data: [Slot::UNINIT; BLOCK_SIZE],
        }))
    }

    /// write index with data
    #[inline]
    pub fn set(&self, index: usize, v: T) {
        unsafe {
            let data = self.data.get_unchecked(index & BLOCK_MASK);
            (*data.value.get()).write(v);
        }
    }

    /// peek the indexed value
    /// not safe if pop out a value when hold the data ref
    #[inline]
    pub unsafe fn peek(&self, index: usize) -> &T {
        let data = self.data.get_unchecked(index & BLOCK_MASK);
        (*data.value.get()).assume_init_ref()
    }

    /// read out indexed value
    /// this would make the underlying data dropped when it get out of scope
    #[inline]
    pub fn get(&self, index: usize) -> T {
        unsafe {
            let data = self.data.get_unchecked(index & BLOCK_MASK);
            (*data.value.get()).assume_init_read()
        }
    }

    /// bulk get until the end of this block
    /// you must make sure that end is not passing the end of this block
    /// use bulk_end() for the end para
    #[inline]
    pub unsafe fn bulk_get<V: Extend<T>>(&self, start: usize, end: usize, vec: &mut V) -> usize {
        let size = end.wrapping_sub(start);
        // vec.reserve(size);
        for i in 0..size {
            vec.extend(Some(self.get(start + i)));
        }
        size
    }
}

/// return the bulk end with in the block
#[inline]
pub fn bulk_end(start: usize, end: usize, mut expect: usize) -> usize {
    if expect == 0 {
        expect = BLOCK_SIZE;
    }
    let size0 = end.wrapping_sub(start);
    let size1 = BLOCK_SIZE - (start & BLOCK_MASK);
    // only pop within a block
    expect = cmp::min(size0, expect);
    expect = cmp::min(size1, expect);
    start.wrapping_add(expect)
}

// impl<T> Index<usize> for BlockNode<T> {
// type Output = T;
//
// read the indexed value
// different from get() that the data would not dropped
// #[inline]
// fn index<'a>(&'a self, index: usize) -> &'a T {
// unsafe { &*self.data.ptr().offset((index & BLOCK_MASK) as isize) }
// }
// }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_node_sanity() {
        let b = unsafe { &*BlockNode::<Box<usize>>::new() };
        for i in 0..BLOCK_SIZE {
            b.set(i, Box::new(i));
            // assert_eq!(i, *b[i]);
            // this would drop the underlying data
            assert_eq!(i, *b.get(i));
        }
    }
}
