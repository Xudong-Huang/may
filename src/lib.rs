#![cfg_attr(nightly, feature(specialization))]
#![cfg_attr(nightly, feature(core_intrinsics))]

#[macro_use]
extern crate log;

extern crate net2;
// windows platform not use this crate
#[allow(unused_extern_crates)]
extern crate smallvec;
extern crate crossbeam;
extern crate may_queue;
extern crate generator;

mod io;
mod join;
mod park;
mod pool;
mod local;
mod sleep;
mod scoped;
mod cancel;
mod config;
mod scheduler;
mod yield_now;
mod coroutine_impl;
mod timeout_list;

pub mod net;
pub mod sync;
pub mod cqueue;
pub mod coroutine;
pub use config::config;
pub use local::LocalKey;
