use std::ops::{Deref, DerefMut};
use smallvec::SmallVec;
use super::sys::SysEvent;

// buffer to receive the system events
pub struct EventsBuf(SmallVec<[SysEvent; 1024]>);

impl EventsBuf {
    pub fn new() -> Self {
        let mut v = SmallVec::new();
        unsafe {
            v.set_len(1024);
        }
        EventsBuf(v)
    }
}

impl Deref for EventsBuf {
    type Target = SmallVec<[SysEvent; 1024]>;
    fn deref(&self) -> &SmallVec<[SysEvent; 1024]> {
        &self.0
    }
}

impl DerefMut for EventsBuf {
    fn deref_mut(&mut self) -> &mut SmallVec<[SysEvent; 1024]> {
        &mut self.0
    }
}
