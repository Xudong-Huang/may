#![cfg_attr(all(nightly, test), feature(test))]

mod atomic;
mod block_node;

pub mod mpsc_seg_queue;
pub mod seg_queue;
pub mod spsc_seg_queue;
pub mod tokio_queue;

pub mod mpmc_bounded;
pub mod mpsc_list;
pub mod mpsc_list_v1;
pub mod spmc;
pub mod spsc;

pub use crate::block_node::BLOCK_SIZE;

#[cfg(test)]
mod test_queue {
    pub trait ScBlockPop<T> {
        fn block_pop(&self) -> T;
    }
}
