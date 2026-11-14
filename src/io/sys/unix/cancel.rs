use crate::cancel::CancelIo;
use crate::scheduler::get_scheduler;
use crate::sync::{AtomicOption, WaitCo};

// The registration is the shared slot of the wait it was armed for, not the
// io data. The `set_io`/`clear` pair is not atomic with a coroutine resume: a
// wait that completes before `subscribe` stores the registration leaves it
// behind for a while (e.g. a zero timeout that fires while the timer is being
// armed). Registering the io data would let such a stale registration take the
// coroutine of whichever wait is current on that io data later; registering
// the wait itself means a stale one can only look into its own emptied slot.
pub struct CancelIoImpl(AtomicOption<WaitCo>);

impl CancelIo for CancelIoImpl {
    type Data = WaitCo;

    fn new() -> Self {
        CancelIoImpl(AtomicOption::none())
    }

    fn set(&self, data: WaitCo) {
        self.0.store(data);
    }

    fn clear(&self) {
        self.0.take();
    }

    fn cancel(&self) -> bool {
        if let Some(wait) = self.0.take() {
            if let Some(co) = wait.take() {
                get_scheduler().schedule(co);
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coroutine_impl::{CoroutineImpl, EventSource};
    use crate::yield_now::yield_with;
    use std::sync::mpsc::{channel, Sender};
    use std::sync::Arc;
    use std::time::Duration;

    struct Capture(Sender<CoroutineImpl>);

    impl EventSource for Capture {
        fn subscribe(&mut self, co: CoroutineImpl) {
            self.0.send(co).unwrap();
        }
    }

    /// A registration left over from a wait that already ended must report
    /// that it took nothing, so `CancelImpl::cancel` falls back to the
    /// coroutine's current wait instead of taking one from the io data.
    #[test]
    fn stale_registration_takes_nothing() {
        let io = CancelIoImpl::new();
        // this is what a wait that already completed leaves behind
        io.set(Arc::new(AtomicOption::none()));
        assert!(!io.cancel());
    }

    /// The registration takes the coroutine of the wait it was armed for, and
    /// only once.
    #[test]
    fn registration_takes_the_armed_wait() {
        let (tx_co, rx_co) = channel();
        let (tx_done, rx_done) = channel();
        crate::go!(move || {
            let capture = Capture(tx_co);
            yield_with(&capture);
            tx_done.send(()).unwrap();
        });

        let co = rx_co
            .recv_timeout(Duration::from_secs(5))
            .expect("the coroutine did not suspend");
        let wait = Arc::new(AtomicOption::some(co));

        let io = CancelIoImpl::new();
        io.set(wait.clone());
        assert!(io.cancel());
        assert!(wait.take().is_none());

        // the scheduled coroutine completes; nothing else is resumed
        assert!(rx_done.recv_timeout(Duration::from_secs(5)).is_ok());

        // the entry is consumed, a later cancel finds nothing
        assert!(!io.cancel());
    }
}
