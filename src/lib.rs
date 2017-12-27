//! # Rust Stackful Coroutine Library
//!
//! May is a high performance stackful coroutine library that can be thought of rust version `goroutine`.
//! You can use it easily to design and develop massive concurrent programs in Rust.
//!
//! ## Features
//!
//! * Stackful coroutine implementation based on stackful `generator`
//! * Support schedule on configurable number of threads for multi-cores
//! * Support coroutine version local storage
//! * Support efficient network async IO
//! * Support efficient timer management
//! * Support standard sync primitives plus semaphore, mpmc channel etc.
//! * Support cancellation of coroutines
//! * Support graceful panic handling that will not affect other coroutines
//! * Support scoped coroutine creation
//! * Support general select for all the coroutine APIs
//! * All the coroutine APIs are compatible with std library semantics
//! * All the coroutine APIs can be safely called in thread context
//!

// #![deny(missing_docs)]
#![cfg_attr(nightly, feature(specialization))]
#![cfg_attr(nightly, feature(core_intrinsics))]

#[macro_use]
#[doc(hidden)]
extern crate log;
#[doc(hidden)]
extern crate net2;
// windows platform not use this crate
#[allow(unused_extern_crates)]
#[doc(hidden)]
extern crate smallvec;
#[doc(hidden)]
extern crate crossbeam;
#[doc(hidden)]
extern crate may_queue;
#[doc(hidden)]
extern crate generator;

mod io;
mod join;
mod park;
mod pool;
mod local;
mod sleep;
#[macro_use]
mod macros;
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
