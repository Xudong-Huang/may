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
extern crate socket2;
// windows platform not use this crate
#[doc(hidden)]
extern crate crossbeam;
#[doc(hidden)]
extern crate generator;
#[doc(hidden)]
extern crate may_queue;
#[allow(unused_extern_crates)]
#[doc(hidden)]
extern crate smallvec;
#[cfg(test)]
#[doc(hidden)]
extern crate tempdir;

#[cfg(windows)]
#[doc(hidden)]
extern crate miow;
#[cfg(windows)]
#[doc(hidden)]
extern crate winapi;

#[cfg(unix)]
#[doc(hidden)]
extern crate libc;
#[cfg(unix)]
#[doc(hidden)]
extern crate nix;

mod cancel;
mod config;
mod join;
mod local;
mod park;
mod pool;
mod sleep;
#[macro_use]
mod macros;
mod coroutine_impl;
mod scheduler;
mod scoped;
mod timeout_list;
mod yield_now;

pub mod coroutine;
pub mod cqueue;
pub mod io;
pub mod net;
pub mod os;
pub mod sync;
pub use config::{config, Config};
pub use local::LocalKey;
