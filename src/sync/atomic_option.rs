pub use may_queue::atomic_option::AtomicOption;

use super::Blocker;
use crate::coroutine_impl::CoroutineImpl;
use crate::timeout_list::TimeoutHandle;

use std::sync::Arc;

/// The shared hand-off handle for one suspended coroutine.
///
/// The coroutine is taken out of it by exactly one of: an unpark, a
/// cancellation, or the scheduler timer armed for that wait. The `Arc` lets
/// the timer and the cancel machinery keep a claim on their own wait after it
/// has ended, without ever reaching a later wait registered elsewhere.
pub(crate) type WaitCo = Arc<AtomicOption<CoroutineImpl>>;

const _: () = assert!(AtomicOption::<CoroutineImpl>::DIRECT_ATOMIC);
const _: () = assert!(AtomicOption::<Arc<Blocker>>::DIRECT_ATOMIC);
const _: () = assert!(AtomicOption::<WaitCo>::DIRECT_ATOMIC);
// the timer handle slots of `park::Park` and `spsc::RecvTimeoutSource`
const _: () = assert!(AtomicOption::<TimeoutHandle<WaitCo>>::DIRECT_ATOMIC);
