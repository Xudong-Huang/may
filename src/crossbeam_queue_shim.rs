use crossbeam::deque::{Stealer, Worker};

pub struct Local<T>(Worker<T>);
impl<T> Local<T> {
    pub fn pop(&self) -> Option<T> {
        self.0.pop()
    }

    pub fn push_back(&self, value: T) {
        self.0.push(value);
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

pub struct Steal<T>(Stealer<T>);

impl<T> Clone for Steal<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> Steal<T> {
    pub fn steal_into(&self, target: &Local<T>) -> Option<T> {
        loop {
            match self.0.steal_batch_and_pop(&target.0) {
                crossbeam::deque::Steal::Empty => return None,
                crossbeam::deque::Steal::Success(v) => return Some(v),
                crossbeam::deque::Steal::Retry => {}
            }
        }
    }
}

pub fn local<T: 'static>() -> (Steal<T>, Local<T>) {
    let worker = Worker::new_fifo();
    let stealer = Steal(worker.stealer());
    (stealer, Local(worker))
}
