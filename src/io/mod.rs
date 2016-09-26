#[cfg(unix)]
#[path = "sys/unix/mod.rs"]
mod sys;

#[cfg(windows)]
#[path = "sys/windows/mod.rs"]
mod sys;

mod event_loop;

pub use self::event_loop::EventLoop;
pub use self::sys::{EventData, Selector, net};

// macro_rules! co_try {
//     ($e:expr) => (match $e {
//         Ok(val) => val,
//         Err(err) => {
//             ::yield_now::set_co_para(&mut co, err);
//             s.schedule_io(co);
//             return;
//         },
//     });
// }
