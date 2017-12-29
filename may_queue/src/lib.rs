#![cfg_attr(nightly, feature(alloc))]
#![cfg_attr(nightly, feature(core_intrinsics))]
#![cfg_attr(all(nightly, test), feature(test))]

mod block_node;

pub mod spsc;
pub mod mpsc_list;
pub mod mpsc_list_v1;
pub mod mpmc_bounded;

pub use block_node::BLOCK_SIZE;

#[cfg(test)]
mod test_queue {
    pub trait ScBlockPop<T> {
        fn block_pop(&self) -> T;
    }

    macro_rules! block_pop_sc_impl {
        // `()` indicates that the macro takes no argument.
        ($queue: ident) => (
            impl<T> ScBlockPop<T> for super::$queue::Queue<T> {
                fn block_pop(&self) -> T {
                    let mut i = 0;
                    loop {
                        if let Some(v) = self.pop() {
                            return v;
                        }

                        if i > 10 {
                            i = 0;
                            ::std::thread::yield_now();
                        }
                        i += 1;
                    }
                }
            }
        )
    }

    block_pop_sc_impl!(spsc);
}
