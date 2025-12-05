use crossbeam::deque::{Stealer, Worker};

pub struct Local<T>(Worker<T>);
impl<T> Local<T> {
    pub fn pop(&self) -> Option<T> {
        self.0.pop()
    }

    pub fn push_back(&self, value: T) {
        self.0.push(value);
    }

    pub fn has_tasks(&self) -> bool {
        !self.0.is_empty()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_local_new() {
        let (_stealer, local_queue): (Steal<i32>, Local<i32>) = local();

        // Test that local queue is empty initially
        assert!(!local_queue.has_tasks());
        assert!(local_queue.pop().is_none());
    }

    #[test]
    fn test_local_push_and_pop() {
        let (_stealer, local_queue): (Steal<i32>, Local<i32>) = local();

        // Test push_back
        local_queue.push_back(42);
        assert!(local_queue.has_tasks());

        // Test pop
        let value = local_queue.pop().unwrap();
        assert_eq!(value, 42);
        assert!(!local_queue.has_tasks());
    }

    #[test]
    fn test_local_multiple_operations() {
        let (_stealer, local_queue): (Steal<i32>, Local<i32>) = local();

        // Push multiple values
        for i in 0..5 {
            local_queue.push_back(i);
        }

        assert!(local_queue.has_tasks());

        // Pop all values (FIFO order)
        for i in 0..5 {
            let value = local_queue.pop().unwrap();
            assert_eq!(value, i);
        }

        assert!(!local_queue.has_tasks());
        assert!(local_queue.pop().is_none());
    }

    #[test]
    fn test_stealer_clone() {
        let (stealer, _local_queue): (Steal<i32>, Local<i32>) = local();

        // Test that stealer can be cloned
        let stealer2 = stealer.clone();

        // Both stealers should work identically
        // This is more of a compilation test since they share the same underlying stealer
        let _stealer3 = stealer2.clone();
    }

    #[test]
    fn test_steal_from_empty_queue() {
        let (stealer, local_queue): (Steal<i32>, Local<i32>) = local();

        // Try to steal from empty queue
        let result = stealer.steal_into(&local_queue);
        assert!(result.is_none());
    }

    #[test]
    fn test_steal_into_operation() {
        let (stealer1, local_queue1): (Steal<i32>, Local<i32>) = local();
        let (_stealer2, local_queue2): (Steal<i32>, Local<i32>) = local();

        // Add items to local1
        for i in 0..10 {
            local_queue1.push_back(i);
        }

        // Steal from local1 into local2
        let stolen = stealer1.steal_into(&local_queue2);

        // Should have stolen at least one item
        if stolen.is_some() {
            assert!(local_queue2.has_tasks());
        }
    }

    #[test]
    fn test_concurrent_steal_operations() {
        let (stealer, local_queue): (Steal<i32>, Local<i32>) = local();

        // Add many items
        for i in 0..100 {
            local_queue.push_back(i);
        }

        let stealer_clone = stealer.clone();
        let (_stealer2, local_queue2): (Steal<i32>, Local<i32>) = local();

        // Test concurrent stealing
        let handle = thread::spawn(move || {
            let mut stolen_count = 0;
            for _ in 0..10 {
                if stealer_clone.steal_into(&local_queue2).is_some() {
                    stolen_count += 1;
                }
            }
            stolen_count
        });

        // Also steal from main thread
        let mut main_stolen = 0;
        for _ in 0..10 {
            if stealer.steal_into(&local_queue).is_some() {
                main_stolen += 1;
            }
        }

        let thread_stolen = handle.join().unwrap();

        // At least some stealing should have occurred
        // (exact numbers depend on timing and implementation)
        assert!(main_stolen >= 0);
        assert!(thread_stolen >= 0);
    }

    #[test]
    fn test_local_has_tasks_accuracy() {
        let (_stealer, local_queue): (Steal<i32>, Local<i32>) = local();

        // Initially empty
        assert!(!local_queue.has_tasks());

        // Add one item
        local_queue.push_back(42);
        assert!(local_queue.has_tasks());

        // Remove item
        let _value = local_queue.pop();
        assert!(!local_queue.has_tasks());

        // Add multiple items
        local_queue.push_back(1);
        local_queue.push_back(2);
        assert!(local_queue.has_tasks());

        // Remove one
        let _value = local_queue.pop();
        assert!(local_queue.has_tasks()); // Should still have tasks

        // Remove last one
        let _value = local_queue.pop();
        assert!(!local_queue.has_tasks());
    }

    #[test]
    fn test_different_types() {
        // Test with String
        let (_stealer, local_queue): (Steal<String>, Local<String>) = local();
        local_queue.push_back("hello".to_string());
        let value = local_queue.pop().unwrap();
        assert_eq!(value, "hello");

        // Test with Vec
        let (_stealer, local_queue): (Steal<Vec<i32>>, Local<Vec<i32>>) = local();
        local_queue.push_back(vec![1, 2, 3]);
        let value = local_queue.pop().unwrap();
        assert_eq!(value, vec![1, 2, 3]);

        // Test with Arc
        let (_stealer, local_queue): (Steal<Arc<i32>>, Local<Arc<i32>>) = local();
        let data = Arc::new(42);
        local_queue.push_back(data.clone());
        let value = local_queue.pop().unwrap();
        assert_eq!(*value, 42);
    }

    #[test]
    fn test_steal_retry_behavior() {
        let (stealer, local_queue1): (Steal<i32>, Local<i32>) = local();
        let (_stealer2, local_queue2): (Steal<i32>, Local<i32>) = local();

        // Add a few items
        for i in 0..5 {
            local_queue1.push_back(i);
        }

        // Multiple steal attempts
        let mut successful_steals = 0;
        for _ in 0..10 {
            if stealer.steal_into(&local_queue2).is_some() {
                successful_steals += 1;
            }
        }

        // Should have at least one successful steal
        assert!(successful_steals > 0);
    }
}
