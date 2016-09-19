use std::mem;
use std::cmp;
use std::thread;
use std::sync::{Mutex, RwLock, Arc};
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::collections::{HashMap, BinaryHeap};
use queue::mpsc_list::Queue as mpsc;
use queue::mpsc_list::Entry;
use sync::AtomicOption;

fn dur_to_ns(dur: Duration) -> u64 {
    // Note that a duration is a (u64, u32) (seconds, nanoseconds) pair
    dur.as_secs()
        .checked_mul(1_000_000_000)
        .and_then(|ns| ns.checked_add((dur.subsec_nanos() as u64)))
        .expect("too big value for sleepping!")
}

// get the current wall clock in ns
fn now() -> u64 {
    0
}

// timeout event data
pub struct TimeoutData<T> {
    time: u64, // the wall clock in ns that the timer expires
    data: T, // the data associate with the timeout event
}

// timeout handler which can be removed/cancelled
pub type TimeoutHandle<T> = Entry<TimeoutData<T>>;

type IntervalList<T> = Arc<mpsc<TimeoutData<T>>>;


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
        let p = |v: &TimeoutData<T>| v.time >= now;
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
    // the timer thread wakeup handler
    wakeup: AtomicOption<thread::Thread>,
}


impl<T> TimeOutList<T> {
    pub fn new() -> Self {
        TimeOutList {
            interval_map: RwLock::new(HashMap::new()),
            timer_bh: Mutex::new(BinaryHeap::new()),
            wakeup: AtomicOption::new(),
        }
    }

    // add a timeout event to the list
    // this can be called in any thread
    pub fn add_timer(&self, dur: Duration, data: T) -> TimeoutHandle<T> {
        let interval = dur_to_ns(dur);
        let time = now() + interval; // TODO: deal with overflow?

        let timeout = TimeoutData {
            time: time,
            data: data,
        };

        {
            // use the read lock protect
            let interval_map_r = self.interval_map.read().unwrap();
            // first get the read locker to get the list
            if let Some(interval_list) = (*interval_map_r).get(&interval) {
                return interval_list.push(timeout);
            }
            // drop the read lock here
        }
        // if the interval list is not there, get the write locker to install the list
        // use the write lock protect
        let mut interval_map_w = self.interval_map.write().unwrap();
        // recheck the interval list in case other thread may install it
        if let Some(interval_list) = (*interval_map_w).get(&interval) {
            return interval_list.push(timeout);
        }

        let interval_list = Arc::new(mpsc::<TimeoutData<T>>::new());
        let ret = interval_list.push(timeout);
        (*interval_map_w).insert(interval, interval_list.clone());
        // drop the write lock here
        mem::drop(interval_map_w);

        {
            // install the new interval list to the binary heap
            let entry = IntervalEntry {
                time: time,
                interval: interval,
                list: interval_list,
            };
            let mut timer_bh = self.timer_bh.lock().unwrap();
            (*timer_bh).push(entry);
        }

        // wake up the timer thread
        self.wakeup.take(Ordering::Relaxed).map(|t| t.unpark());
        ret
    }

    // schedule in the timer thread
    // this will remove all the expired timeout event
    // and call the supplied function with registered data
    // return the time in ns for the next expiration
    fn schedule_timer<F: Fn(T)>(&mut self, now: u64, f: &F) -> Option<u64> {
        loop {
            // first peek the BH to see if there is any timeout event
            {
                let timer_bh = self.timer_bh.lock().unwrap();
                match (*timer_bh).peek() {
                    None => {
                        return None;
                    }
                    Some(entry) => {
                        // the latest timeout event not happened yet
                        if entry.time > now {
                            return Some(entry.time - now);
                        }
                    }
                }
            }

            // pop out the entry
            let mut entry;
            {
                let mut timer_bh = self.timer_bh.lock().unwrap();
                entry = (*timer_bh).pop().unwrap();
            }

            // consume all the timeout event
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
                        // the list is really empty now, we can safely remove it
                        (*interval_map_w).remove(&entry.interval);
                    } else {
                        // release the w lock first, we don't need any more
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

    // the timer thread function
    pub fn run<F: Fn(T)>(&mut self, f: &F) {
        let current_thread = thread::current();
        loop {
            let now = now();
            let next_expire = self.schedule_timer(now, f);
            self.wakeup.swap(current_thread.clone(), Ordering::Relaxed);
            match next_expire {
                Some(time) => {
                    thread::park_timeout(Duration::from_millis(time / 1_000_000));
                }
                None => thread::park(),
            }
        }
    }
}

mod tests {
    use super::*;

    #[test]
    fn test_timeout_list() {
        let l = TimeOutList::<usize>::new();
    }
}
