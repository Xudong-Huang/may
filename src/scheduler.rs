use std::ptr;
use std::mem;
use std::thread;
use std::cell::{UnsafeCell, Cell};
use std::sync::{Once, ONCE_INIT};
use std::sync::mpsc::{channel, Sender};
use queue::BLOCK_SIZE;
use queue::wait_queue::Queue as CoQueue;
use coroutine::CoroutineImpl;

thread_local!{static ID: Cell<usize> = Cell::new(0);}

#[inline]
pub fn get_scheduler() -> &'static Scheduler {
    let workers = 4;
    static mut sched: *const Scheduler = 0 as *const _;
    static ONCE: Once = ONCE_INIT;
    ONCE.call_once(|| {
        let b: Box<Scheduler> = Scheduler::new(workers);
        let (tx, rx) = channel();
        b.set_co_tx(tx.clone());
        unsafe {
            sched = Box::into_raw(b);
        }

        // one thread to collect coroutine created from normal thread context
        // this thread is a proxy for normal thread
        // do we have a better design?
        thread::spawn(move || {
            let s = unsafe { &*sched };
            // we can't call schedule here recursively
            rx.iter().fold((), |_, co| s.schedule_0(co));
        });

        // run the scheduler in background
        for id in 0..workers {
            thread::spawn(move || {
                ID.with(|m_id| m_id.set(id));
                let s = unsafe { &*sched };
                s.run(id);
            });
        }
    });
    unsafe { &*sched }
}

pub struct Scheduler {
    ready_list: CoQueue<CoroutineImpl>,
    tx: UnsafeCell<Sender<CoroutineImpl>>,
}

impl Scheduler {
    pub fn new(workers: usize) -> Box<Self> {
        Box::new(Scheduler {
            ready_list: CoQueue::new(workers),
            tx: UnsafeCell::new(unsafe { mem::uninitialized() }),
        })
    }

    #[allow(dead_code)]
    pub fn run(&self, id: usize) {
        let mut vec = Vec::with_capacity(BLOCK_SIZE);
        loop {
            let size = self.ready_list.bulk_pop(id, &mut vec);
            vec.drain(0..size).fold((), |_, mut coroutine| {
                let event_subscriber = coroutine.resume().unwrap();
                // the coroutine will sink into the subscriber's resouce
                // basically it just register the coroutine somewhere within the 'resource'
                // 'resource' is just any type that live within the coroutine's stack
                event_subscriber.subscribe(coroutine);
            });
        }
    }

    /// put the coroutine to ready list so that next time it can be scheduled
    #[inline]
    pub fn schedule(&self, co: CoroutineImpl) {
        // only worker thread has none zero id
        let id = ID.with(|m_id| m_id.get());
        if id == 0 {
            // unsafe to push the co to the list so we make it through a mpsc channel
            let tx = unsafe { &*self.tx.get() };
            tx.send(co).unwrap();
            return;
        }
        self.ready_list.push(id, co);
    }

    /// this function is only for queue slot 0, used only by the proxy thread
    #[inline]
    fn schedule_0(&self, co: CoroutineImpl) {
        self.ready_list.push(0, co);
    }

    // set the tx for normal thread
    fn set_co_tx(&self, tx: Sender<CoroutineImpl>) {
        unsafe {
            ptr::write(self.tx.get(), tx);
        }
    }
}
