#![feature(core_intrinsics)]
#[macro_use]
extern crate log;

extern crate net2;
extern crate queue;
extern crate smallvec;
extern crate generator;

mod io;
mod join;
mod park;
mod pool;
mod sync;
mod local;
mod sleep;
mod scoped;
mod scheduler;
mod yield_now;
mod coroutine;
mod timeout_list;

pub mod net;
pub use sleep::sleep;
pub use scoped::scope;
pub use join::JoinHandle;
pub use yield_now::yield_now;
pub use scheduler::scheduler_set_workers;
pub use coroutine::{Builder, spawn, park, park_timeout, current};
