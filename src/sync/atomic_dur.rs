use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

// 0 encodes `None` (no timeout). A timeout is stored in whole milliseconds,
// rounded up and at least 1: truncating would turn any timeout under 1 ms
// into 0, which reads back as "no timeout" and waits forever.
fn encode(dur: Option<Duration>) -> usize {
    match dur {
        None => 0,
        Some(d) => {
            let ms = d.as_nanos().div_ceil(1_000_000).max(1);
            usize::try_from(ms).unwrap_or(usize::MAX)
        }
    }
}

// atomic duration in milli seconds
#[derive(Debug)]
pub struct AtomicDuration(AtomicUsize);

impl AtomicDuration {
    pub fn new(dur: Option<Duration>) -> Self {
        AtomicDuration(AtomicUsize::new(encode(dur)))
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
        self.0.store(encode(dur), Ordering::Relaxed);
    }

    #[inline]
    pub fn take(&self) -> Option<Duration> {
        match self.0.swap(0, Ordering::Relaxed) {
            0 => None,
            d => Some(Duration::from_millis(d as u64)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_timeouts_are_not_lost() {
        let d = AtomicDuration::new(None);
        assert_eq!(d.take(), None);
        for (given, kept) in [
            (Duration::ZERO, 1),
            (Duration::from_nanos(1), 1),
            (Duration::from_micros(500), 1),
            (Duration::from_millis(1), 1),
            (Duration::from_micros(1001), 2),
            (Duration::from_millis(50), 50),
        ] {
            d.store(Some(given));
            assert_eq!(d.take(), Some(Duration::from_millis(kept)), "{given:?}");
        }
        assert_eq!(
            AtomicDuration::new(Some(Duration::from_micros(10))).take(),
            Some(Duration::from_millis(1))
        );
    }
}
