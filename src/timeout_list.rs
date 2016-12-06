extern crate time;
use std::mem;
use std::cmp;
use std::thread;
use std::sync::{Mutex, RwLock, Arc};
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::collections::{HashMap, BinaryHeap};
use queue::mpsc_list::Queue as mpsc;
use queue::mpsc_list_v1::Queue as TimeoutList;
use queue::mpsc_list_v1::Entry;
use sync::AtomicOption;
use self::time::precise_time_ns;

const NANOS_PER_MILLI: u64 = 1_000_000;
const NANOS_PER_SEC: u64 = 1_000_000_000;

const HASH_CAP: usize = 1024;

#[inline]
fn dur_to_ns(dur: Duration) -> u64 {
    // Note that a duration is a (u64, u32) (seconds, nanoseconds) pair
    dur.as_secs().saturating_mul(NANOS_PER_SEC).saturating_add(dur.subsec_nanos() as u64)
}

#[inline]
fn ns_to_dur(ns: u64) -> Duration {
    Duration::new(ns / NANOS_PER_SEC, (ns % NANOS_PER_SEC) as u32)
}

#[inline]
pub fn ns_to_ms(ns: u64) -> u64 {
    (ns + NANOS_PER_MILLI - 1) / NANOS_PER_MILLI as u64
}

// get the current wall clock in ns
#[inline]
pub fn now() -> u64 {
    precise_time_ns()
}

// timeout event data
pub struct TimeoutData<T> {
    time: u64, // the wall clock in ns that the timer expires
    pub data: T, // the data associate with the timeout event
}

// timeout handler which can be removed/cancelled
pub type TimeoutHandle<T> = Entry<TimeoutData<T>>;

type IntervalList<T> = Arc<TimeoutList<TimeoutData<T>>>;


// this is the data type that used by the binary heap to get the latest timer
struct IntervalEntry<T> {
    time: u64, // the head timeout value in the list, should be latest
    list: IntervalList<T>, // point to the inerval list
    interval: u64,
}

impl<T> IntervalEntry<T> {
    // trigger the timeout event with the supplying function
    // return next expire time
    pub fn pop_timeout<F>(&self, now: u64, f: &F) -> Option<u64>
        where F: Fn(T)
    {
        let p = |v: &TimeoutData<T>| v.time <= now;
        loop {
            match self.list.pop_if(&p) {
                Some(timeout) => f(timeout.data),
                None => break,
            }
        }
        self.list.peek().map(|t| t.time)
    }
}

impl<T> PartialEq for IntervalEntry<T> {
    fn eq(&self, other: &Self) -> bool {
        self.time == other.time
    }
}

impl<T> Eq for IntervalEntry<T> {}

impl<T> PartialOrd for IntervalEntry<T> {
    fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> cmp::Ord for IntervalEntry<T> {
    fn cmp(&self, other: &Self) -> cmp::Ordering {
        if self.time < other.time {
            return cmp::Ordering::Greater;
        } else if self.time > other.time {
            return cmp::Ordering::Less;
        }
        cmp::Ordering::Equal
    }
}

// the timeout list data structure
pub struct TimeOutList<T> {
    // interval based hash map, protected by rw lock
    interval_map: RwLock<HashMap<u64, IntervalList<T>>>,
    // a priority queue, each element is the head of a mpsc queue
    timer_bh: Mutex<BinaryHeap<IntervalEntry<T>>>,
}

impl<T> TimeOutList<T> {
    pub fn new() -> Self {
        TimeOutList {
            interval_map: RwLock::new(HashMap::with_capacity(HASH_CAP)),
            timer_bh: Mutex::new(BinaryHeap::new()),
        }
    }

    fn install_timer_bh(&self, entry: IntervalEntry<T>) {
        let mut timer_bh = self.timer_bh.lock().unwrap();
        (*timer_bh).push(entry);
        // lock drop here
    }

    // add a timeout event to the list
    // this can be called in any thread
    // return true if we need to recal next expire
    pub fn add_timer(&self, dur: Duration, data: T) -> (TimeoutHandle<T>, bool) {
        let interval = dur_to_ns(dur);
        let time = now() + interval; // TODO: deal with overflow?
        //println!("add timer = {:?}", time);

        let timeout = TimeoutData {
            time: time,
            data: data,
        };

        let interval_list = {
            // use the read lock protect
            let interval_map_r = self.interval_map.read().unwrap();
            (*interval_map_r).get(&interval).map(|l| l.clone())
            // drop the read lock here
        };

        if let Some(interval_list) = interval_list {
            let (handle, is_head) = interval_list.push(timeout);
            if is_head {
                // install the interval list to the binary heap
                self.install_timer_bh(IntervalEntry {
                    time: time,
                    interval: interval,
                    list: interval_list,
                });
            }
            return (handle, is_head);
        }

        // if the interval list is not there, get the write locker to install the list
        // use the write lock protect
        let mut interval_map_w = self.interval_map.write().unwrap();
        // recheck the interval list in case other thread may install it
        if let Some(interval_list) = (*interval_map_w).get(&interval) {
            let (handle, is_head) = interval_list.push(timeout);
            if is_head {
                // this rarely happens
                self.install_timer_bh(IntervalEntry {
                    time: time,
                    interval: interval,
                    list: interval_list.clone(),
                });
            }
            return (handle, is_head);
        }

        let interval_list = Arc::new(TimeoutList::<TimeoutData<T>>::new());
        let ret = interval_list.push(timeout).0;
        (*interval_map_w).insert(interval, interval_list.clone());
        // drop the write lock here
        mem::drop(interval_map_w);

        // install the new interval list to the binary heap
        self.install_timer_bh(IntervalEntry {
            time: time,
            interval: interval,
            list: interval_list,
        });

        (ret, true)
    }

    // schedule in the timer thread
    // this will remove all the expired timeout event
    // and call the supplied function with registered data
    // return the time in ns for the next expiration
    pub fn schedule_timer<F: Fn(T)>(&self, now: u64, f: &F) -> Option<u64> {
        loop {
            // first peek the BH to see if there is any timeout event
            {
                let timer_bh = self.timer_bh.lock().unwrap();
                match (*timer_bh).peek() {
                    // the latest timeout event not happened yet
                    Some(entry) if entry.time > now => return Some(entry.time - now),
                    None => return None,
                    _ => {}
                }
            }
            // pop out the entry
            let mut entry;
            {
                let mut timer_bh = self.timer_bh.lock().unwrap();
                entry = (*timer_bh).pop().unwrap();
            }

            // consume all the timeout event
            // println!("interval = {:?}", entry.interval);
            match entry.pop_timeout(now, f) {
                Some(time) => {
                    entry.time = time;
                    // repush the entry
                    let mut timer_bh = self.timer_bh.lock().unwrap();
                    (*timer_bh).push(entry);
                }

                None => {
                    // if the inteval list is empty, need to delete it
                    let mut interval_map_w = self.interval_map.write().unwrap();
                    // recheck if the interval list is empty, other thread may append data to it
                    if entry.list.is_empty() {
                        // if the len of the hash map is big enough just leave the queue there
                        if (*interval_map_w).len() > HASH_CAP {
                            // the list is really empty now, we can safely remove it
                            (*interval_map_w).remove(&entry.interval);
                        }
                    } else {
                        // release the w lock first, we don't need it any more
                        mem::drop(interval_map_w);
                        // the list is push some data by other thread
                        entry.time = entry.list.peek().unwrap().time;
                        // repush the entry
                        let mut timer_bh = self.timer_bh.lock().unwrap();
                        (*timer_bh).push(entry);
                    }
                }
            }
        }
    }
}

pub struct TimerThread<T> {
    timer_list: TimeOutList<T>,
    // collect the remove request
    remove_list: mpsc<TimeoutHandle<T>>,
    // the timer thread wakeup handler
    wakeup: AtomicOption<thread::Thread>,
}

impl<T> TimerThread<T> {
    pub fn new() -> Self {
        TimerThread {
            timer_list: TimeOutList::new(),
            remove_list: mpsc::new(),
            wakeup: AtomicOption::none(),
        }
    }

    pub fn add_timer(&self, dur: Duration, data: T) -> TimeoutHandle<T> {
        let (h, is_recal) = self.timer_list.add_timer(dur, data);
        // wake up the timer thread if it's a new queue
        if is_recal {
            self.wakeup.take(Ordering::Relaxed).map(|t| t.unpark());
        }
        h
    }

    pub fn del_timer(&self, handle: TimeoutHandle<T>) {
        self.remove_list.push(handle);
        self.wakeup.take(Ordering::Relaxed).map(|t| t.unpark());
    }

    // the timer thread function
    pub fn run<F: Fn(T)>(&self, f: &F) {
        let current_thread = thread::current();
        loop {
            while let Some(h) = self.remove_list.pop() {
                h.remove();
            }
            // we must register the thread handle first
            // or there will be no signal to wakeup the timer thread
            self.wakeup.swap(current_thread.clone(), Ordering::Relaxed);

            if !self.remove_list.is_empty() {
                self.wakeup.take(Ordering::Relaxed).map(|t| t.unpark());
            }

            match self.timer_list.schedule_timer(now(), f) {
                Some(time) => thread::park_timeout(ns_to_dur(time)),
                None => thread::park(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn test_timeout_list() {
        let timer = Arc::new(TimerThread::<usize>::new());
        let t = timer.clone();
        let f = |data: usize| {
            println!("timeout data:{:?}", data);
        };
        thread::spawn(move || t.run(&f));
        let t1 = timer.clone();
        thread::spawn(move || {
            t1.add_timer(Duration::from_millis(1000), 50);
            t1.add_timer(Duration::from_millis(1000), 60);
            t1.add_timer(Duration::from_millis(1400), 70);
        });
        thread::sleep(Duration::from_millis(10));
        timer.add_timer(Duration::from_millis(1000), 10);
        timer.add_timer(Duration::from_millis(500), 40);
        timer.add_timer(Duration::from_millis(1200), 20);
        thread::sleep(Duration::from_millis(100));
        timer.add_timer(Duration::from_millis(1000), 30);

        thread::sleep(Duration::from_millis(1500));
    }
}
