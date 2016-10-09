macro_rules! co_try {
    ($s: expr, $co: expr, $e:expr) => (match $e {
        Ok(val) => val,
        Err(err) => {
            let mut co = $co;
            ::yield_now::set_co_para(&mut co, err);
            $s.schedule_io(co);
            return;
        }
    })
}

#[cfg(unix)]
#[path = "sys/unix/mod.rs"]
mod sys;

#[cfg(windows)]
#[path = "sys/windows/mod.rs"]
mod sys;

mod event_buf;
mod event_loop;

pub use self::event_loop::EventLoop;
pub use self::sys::{EventData, Selector, net};
