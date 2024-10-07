use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

// atomic duration in milli seconds
#[derive(Debug)]
pub struct AtomicDuration(AtomicUsize);

impl AtomicDuration {
    pub fn new(dur: Option<Duration>) -> Self {
        let dur = match dur {
            None => 0,
            Some(d) => dur_to_ms(d) as usize,
        };

        AtomicDuration(AtomicUsize::new(dur))
    }

    #[inline]
    #[cfg(feature = "io_timeout")]
    pub fn get(&self) -> Option<Duration> {
        match self.0.load(Ordering::Relaxed) {
            0 => None,
            d => Some(Duration::from_millis(d as u64)),
        }
    }

    #[inline]
    pub fn store(&self, dur: Option<Duration>) {
        let timeout = match dur {
            None => 0,
            Some(d) => dur_to_ms(d) as usize,
        };

        self.0.store(timeout, Ordering::Relaxed);
    }

    #[inline]
    pub fn take(&self) -> Option<Duration> {
        match self.0.swap(0, Ordering::Relaxed) {
            0 => None,
            d => Some(Duration::from_millis(d as u64)),
        }
    }
}

fn dur_to_ms(dur: Duration) -> u64 {
    dur.as_millis() as u64
}
