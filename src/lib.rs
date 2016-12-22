#![feature(core_intrinsics)]
#![feature(specialization)]
#![feature(rc_raw)]

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
mod local;
mod sleep;
mod scoped;
mod config;
mod scheduler;
mod yield_now;
mod coroutine;
mod timeout_list;

pub mod net;
pub mod sync;
pub use sleep::sleep;
pub use scoped::scope;
pub use join::JoinHandle;
pub use yield_now::yield_now;
pub use config::scheduler_config;
pub use coroutine::{Builder, spawn, park, park_timeout, current};
