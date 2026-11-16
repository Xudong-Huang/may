//! Completion-based `io_uring` backend for Linux.
//!
//! This tree is selected as `crate::io::sys` when the `io_uring` feature is
//! enabled on Linux, exactly like `sys/unix` and `sys/windows`. It mirrors the
//! Windows IOCP backend: each waiter owns a per-request `EventData`, an adapter
//! submits an SQE, and the worker's ring dispatches the CQE back to the
//! suspended coroutine.
//!
//! During bring-up (M1–M3) the rest of the backend is still the epoll tree: the
//! modules below re-export it temporarily so the crate keeps working while the
//! ring layer and, later, the completion adapters are built. M4 replaces the
//! bridge with the real `EventData`/`Selector`/adapter modules.

#[allow(dead_code)] // consumed by the selector in M2
pub(crate) mod ring;

// Request state and selector cores. They are nested while the epoll bridge is
// still the active `sys`; M4 removes the bridge and promotes these types.
#[allow(dead_code)]
pub(crate) mod event;
#[allow(dead_code)]
pub(crate) mod selector;

// Temporary bridge: compile and re-export the existing epoll backend until the
// io_uring selector and adapters are ready. The nested path keeps the unix
// module's internal relative imports intact.
#[path = "../unix/mod.rs"]
mod epoll_bridge;

#[cfg(feature = "io_cancel")]
pub(crate) use epoll_bridge::cancel;
pub(crate) use epoll_bridge::{
    add_socket, co_io_result, mod_socket, EventData, Selector, SysEvent,
};
pub use epoll_bridge::{co_io, net, wait_io, IoData};
