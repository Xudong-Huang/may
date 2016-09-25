#[cfg(unix)]
#[path = "sys/unix/mod.rs"]
mod sys;

#[cfg(windows)]
#[path = "sys/windows/mod.rs"]
mod sys;

mod event_loop;

pub use self::event_loop::EventLoop;
pub use self::sys::{EventData, Selector, add_socket};
