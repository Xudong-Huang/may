#![cfg_attr(all(nightly, test), feature(test))]

mod atomic;

pub mod mpsc_seg_queue;
pub mod seg_queue;
pub mod tokio_queue;

pub mod mpmc_bounded;
pub mod mpsc;
pub mod mpsc_list;
pub mod mpsc_list_v1;
pub mod spmc;
pub mod spsc;
pub mod unordered_mpsc;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub mod cpu_local;

#[cfg(test)]
mod test_queue {
    pub trait ScBlockPop<T> {
        fn block_pop(&self) -> T;
    }
}
