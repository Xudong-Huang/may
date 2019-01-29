use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

// atomic duration in milli seconds
#[derive(Debug)]
pub struct AtomicDuration(AtomicUsize);

impl AtomicDuration {
    pub fn new(dur: Option<Duration>) -> Self {
        let dur = match dur {
            None => 0,
            Some(d) => d.as_millis() as usize,
        };

        AtomicDuration(AtomicUsize::new(dur))
    }

    #[inline]
    pub fn get(&self) -> Option<Duration> {
        match self.0.load(Ordering::Relaxed) {
            0 => None,
            d => Some(Duration::from_millis(d as u64)),
        }
    }

    #[inline]
    pub fn swap(&self, dur: Option<Duration>) -> Option<Duration> {
        let timeout = match dur {
            None => 0,
            Some(d) => d.as_millis() as usize,
        };

        match self.0.swap(timeout, Ordering::Relaxed) {
            0 => None,
            d => Some(Duration::from_millis(d as u64)),
        }
    }
}
