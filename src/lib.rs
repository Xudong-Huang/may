//! # A library for programming stackful coroutines in Rust.
//!
//! May is a high-performant library for programming stackful coroutines with which
//! you can easily develop and maintain massive concurrent programs. It can be thought
//! as the Rust version of the popular Goroutine.
//!
//! ## Quick Start
//!
//! ### Safe Coroutine Spawning (Recommended)
//! ```rust
//! use may::coroutine::spawn_safe;
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let handle = spawn_safe(|| {
//!         println!("Hello from a safe coroutine!");
//!         42
//!     })?;
//!     
//!     let result = match handle.join() {
//!         Ok(val) => val,
//!         Err(e) => {
//!             eprintln!("Coroutine panicked: {:?}", e);
//!             return Err("Coroutine execution failed".into());
//!         }
//!     };
//!     println!("Result: {}", result);
//!     Ok(())
//! }
//! ```
//!
//! ### Advanced Configuration
//! ```rust
//! use may::coroutine::{SafeBuilder, SafetyLevel};
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let handle = SafeBuilder::new()
//!         .safety_level(SafetyLevel::Strict)
//!         .stack_size(1024 * 1024)
//!         .name("worker")
//!         .spawn_safe(|| "Safe coroutine with configuration!")?;
//!     
//!     match handle.join() {
//!         Ok(result) => println!("{}", result),
//!         Err(e) => {
//!             eprintln!("Coroutine panicked: {:?}", e);
//!             return Err("Coroutine execution failed".into());
//!         }
//!     }
//!     Ok(())
//! }
//! ```
//!
//! ## Features
//! * **Safe coroutine spawning** with compile-time and runtime safety guarantees;
//! * **Comprehensive safety infrastructure** with TLS safety and stack overflow protection;
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
//!
//! ## Safety Levels
//!
//! May provides configurable safety levels to balance safety and performance:
//!
//! - [`SafetyLevel::Strict`]: Maximum safety with comprehensive runtime validation
//! - [`SafetyLevel::Balanced`]: Good safety with minimal performance overhead (recommended)
//! - [`SafetyLevel::Permissive`]: Basic safety for performance-critical code
//! - [`SafetyLevel::Development`]: Enhanced debugging and validation for development
//!
//! [`SafetyLevel::Strict`]: safety::SafetyLevel::Strict
//! [`SafetyLevel::Balanced`]: safety::SafetyLevel::Balanced
//! [`SafetyLevel::Permissive`]: safety::SafetyLevel::Permissive
//! [`SafetyLevel::Development`]: safety::SafetyLevel::Development

// #![deny(missing_docs)]

#[macro_use]
extern crate log;

mod cancel;
mod config;
mod join;
mod likely;
mod local;
mod park;
mod pool;
mod sleep;
#[macro_use]
mod macros;
mod coroutine_impl;
pub mod safety;
mod scheduler;
mod scoped;
mod timeout_list;
mod yield_now;

#[cfg(feature = "crossbeam_queue_steal")]
mod crossbeam_queue_shim;

pub mod coroutine;
pub mod cqueue;
pub mod io;
pub mod net;
pub mod os;
pub mod sync;
pub use crate::config::{config, Config};
pub use crate::local::LocalKey;
// re-export may_queue
pub use may_queue as queue;
