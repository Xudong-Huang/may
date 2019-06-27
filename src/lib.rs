//! # A library for programming stackful coroutines in Rust.
//!
//! May is a high-performant library for programming stackful coroutines with which
//! you can easily develop and maintain massive concurrent programs. It can be thought
//! as the Rust version of the popular Goroutine.
//!
//! ## Features
//! * The stackful coroutine's implementation is based on [generator][generator];
//! * Support schedule on a configurable number of threads for multi-core systems;
//! * Support coroutine's version of a local storage ([CLS][cls]);
//! * Support efficient asynchronous network I/O;
//! * Support efficient timer management;
//! * Support standard synchronization primitives, a semaphore, an MPMC channel, etc;
//! * Support cancellation of coroutines;
//! * Support graceful panic handling that will not affect other coroutines;
//! * Support scoped coroutine creation;
//! * Support general selection for all the coroutine's API;
//! * All the coroutine's API are compatible with the standard library semantics;
//! * All the coroutine's API can be safely called in multi-threaded context;
//! * Both stable, beta, and nightly channels are supported;
//! * Both x86_64 GNU/Linux, x86_64 Windows, x86_64 Mac OS are supported.

// #![deny(missing_docs)]
#![allow(unused_extern_crates)]
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
#[doc(hidden)]
extern crate num_cpus;
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
pub use crate::config::{config, Config};
pub use crate::local::LocalKey;
