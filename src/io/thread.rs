//! we are using a thread local proxy coroutine to send the io request
//!

use std::sync::Arc;

use crate::coroutine::spawn;
use crate::coroutine_impl::{EventResult, EventSubscriber};
use crate::sync::mpsc::{channel, Sender};
use crate::sync::AtomicOption;

use generator::{co_get_yield, co_yield_with};

thread_local! {
    // SAFETY: thread and coroutine would not run in parallel
    pub static ASSOCIATED_IO_RET: Arc<AtomicOption<Box<EventResult>>> = Arc::new(AtomicOption::none());
    pub static PROXY_CO_SENDER: Sender<EventSubscriber> = {
        let (tx, rx) = channel();
        let parker = std::thread::current();
        let io_ret = ASSOCIATED_IO_RET.with(|r| { r.clone() });
        // this is a proxy coroutine
        let _co = unsafe { spawn(move || {
            // the coroutine would be gone if the thread exit
            while let Ok(es) = rx.recv() {
                co_yield_with(es);
                if let Some(r) = co_get_yield::<EventResult>() {
                    io_ret.store(Box::new(r));
                }
                // wake up the master thread
                parker.unpark();
            }
        })};
        tx
    };
}
