// Copyright (c) 2010-2011 Dmitry Vyukov. All rights reserved.
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions are met:
//
//    1. Redistributions of source code must retain the above copyright notice,
//       this list of conditions and the following disclaimer.
//
//    2. Redistributions in binary form must reproduce the above copyright
//       notice, this list of conditions and the following disclaimer in the
//       documentation and/or other materials provided with the distribution.
//
// THIS SOFTWARE IS PROVIDED BY DMITRY VYUKOV "AS IS" AND ANY EXPRESS OR IMPLIED
// WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF
// MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT
// SHALL DMITRY VYUKOV OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT,
// INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
// LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR
// PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF
// LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE
// OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF
// ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
//
// The views and conclusions contained in the software and documentation are
// those of the authors and should not be interpreted as representing official
// policies, either expressed or implied, of Dmitry Vyukov.
//

#![allow(missing_docs, dead_code)]

// http://www.1024cores.net/home/lock-free-algorithms/queues/bounded-mpmc-queue

// This queue is copy pasted from old rust stdlib.

use std::cell::UnsafeCell;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::sync::Arc;

use crossbeam::utils::CachePadded;

struct Node<T> {
    sequence: AtomicUsize,
    value: Option<T>,
}

unsafe impl<T: Send> Send for Node<T> {}
unsafe impl<T: Sync> Sync for Node<T> {}

struct State<T> {
    enqueue_pos: CachePadded<AtomicUsize>,
    buffer: Vec<UnsafeCell<Node<T>>>,
    dequeue_pos: CachePadded<AtomicUsize>,
    mask: usize,
}

unsafe impl<T: Send> Send for State<T> {}
unsafe impl<T: Sync> Sync for State<T> {}

pub struct Queue<T> {
    state: Arc<State<T>>,
}

impl<T: Send> State<T> {
    fn with_capacity(capacity: usize) -> State<T> {
        let capacity = if capacity < 2 || (capacity & (capacity - 1)) != 0 {
            if capacity < 2 {
                2
            } else {
                // use next power of 2 as capacity
                capacity.next_power_of_two()
            }
        } else {
            capacity
        };
        let buffer = (0..capacity)
            .map(|i| {
                UnsafeCell::new(Node {
                    sequence: AtomicUsize::new(i),
                    value: None,
                })
            })
            .collect::<Vec<_>>();
        State {
            buffer,
            mask: capacity - 1,
            enqueue_pos: AtomicUsize::new(0).into(),
            dequeue_pos: AtomicUsize::new(0).into(),
        }
    }

    fn push(&self, value: T) -> Result<(), T> {
        let mask = self.mask;
        let mut pos = self.enqueue_pos.load(Relaxed);
        loop {
            let node = unsafe { &mut *((self.buffer[pos & mask]).get()) };
            let seq = node.sequence.load(Acquire);

            match seq.cmp(&pos) {
                std::cmp::Ordering::Equal => {
                    match self
                        .enqueue_pos
                        .compare_exchange(pos, pos + 1, Relaxed, Relaxed)
                    {
                        Ok(_) => {
                            node.value = Some(value);
                            node.sequence.store(pos + 1, Release);
                            break;
                        }
                        Err(enqueue_pos) => pos = enqueue_pos,
                    }
                }
                std::cmp::Ordering::Less => return Err(value),
                std::cmp::Ordering::Greater => pos = self.enqueue_pos.load(Relaxed),
            }
        }
        Ok(())
    }

    fn pop(&self) -> Option<T> {
        let mask = self.mask;
        let mut pos = self.dequeue_pos.load(Relaxed);
        loop {
            let node = unsafe { &mut *((self.buffer[pos & mask]).get()) };
            let seq = node.sequence.load(Acquire);

            match seq.cmp(&(pos + 1)) {
                std::cmp::Ordering::Equal => {
                    match self
                        .dequeue_pos
                        .compare_exchange(pos, pos + 1, Relaxed, Relaxed)
                    {
                        Ok(_) => {
                            let value = node.value.take();
                            node.sequence.store(pos + mask + 1, Release);
                            return value;
                        }
                        Err(dequeue_pos) => pos = dequeue_pos,
                    }
                }
                std::cmp::Ordering::Less => return None,
                std::cmp::Ordering::Greater => pos = self.dequeue_pos.load(Relaxed),
            }
        }
    }
}

impl<T: Send> Queue<T> {
    pub fn with_capacity(capacity: usize) -> Queue<T> {
        Queue {
            state: Arc::new(State::with_capacity(capacity)),
        }
    }

    pub fn push(&self, value: T) -> Result<(), T> {
        self.state.push(value)
    }

    pub fn pop(&self) -> Option<T> {
        self.state.pop()
    }
}

impl<T: Send> Clone for Queue<T> {
    fn clone(&self) -> Queue<T> {
        Queue {
            state: self.state.clone(),
        }
    }
}

#[cfg(all(nightly, test))]
mod bench {
    extern crate test;
    use self::test::Bencher;

    use super::Queue;
    use std::sync::mpsc::channel;
    use std::thread;

    #[bench]
    fn bounded_mpmc(b: &mut Bencher) {
        b.iter(|| {
            let total_work = 1_000_000;
            let nthreads = 1;
            let nmsgs = total_work / nthreads;
            let q = Queue::with_capacity(nthreads * nmsgs);
            assert_eq!(None, q.pop());
            let (tx, rx) = channel();

            for _ in 0..nthreads {
                let q = q.clone();
                let tx = tx.clone();
                thread::spawn(move || {
                    let q = q;
                    for i in 0..nmsgs {
                        assert!(q.push(i).is_ok());
                    }
                    tx.send(()).unwrap();
                });
            }

            let mut completion_rxs = vec![];
            for _ in 0..nthreads {
                let (tx, rx) = channel();
                completion_rxs.push(rx);
                let q = q.clone();
                thread::spawn(move || {
                    let q = q;
                    let mut i = 0;
                    loop {
                        match q.pop() {
                            None => {}
                            Some(_) => {
                                i += 1;
                                if i == nmsgs {
                                    break;
                                }
                            }
                        }
                    }
                    tx.send(i).unwrap();
                });
            }

            for rx in completion_rxs.iter_mut() {
                assert_eq!(nmsgs, rx.recv().unwrap());
            }
            for _ in 0..nthreads {
                rx.recv().unwrap();
            }
        });
    }
}
