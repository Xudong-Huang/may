//! coroutine io utilities

#[cfg(unix)]
#[path = "sys/unix/mod.rs"]
pub(crate) mod sys;

#[cfg(windows)]
#[path = "sys/windows/mod.rs"]
pub(crate) mod sys;

// export the generic IO wrapper
pub mod co_io_err;

mod event_loop;
pub(crate) mod split_io;
pub(crate) mod thread;

use std::ops::Deref;

pub(crate) use self::event_loop::EventLoop;
#[cfg(feature = "io_cancel")]
pub(crate) use self::sys::cancel;
pub use self::sys::co_io::CoIo;
#[cfg(unix)]
pub use self::sys::wait_io::{WaitIo, WaitIoWaker};
pub use self::sys::IoData;
pub(crate) use self::sys::{add_socket, net, Selector};
pub use split_io::{SplitIo, SplitReader, SplitWriter};

pub trait AsIoData {
    fn as_io_data(&self) -> &IoData;
}

// an option type that implement deref
struct OptionCell<T>(Option<T>);

impl<T> Deref for OptionCell<T> {
    type Target = T;
    fn deref(&self) -> &T {
        match self.0.as_ref() {
            Some(d) => d,
            None => panic!("no data to deref for OptionCell"),
        }
    }
}

impl<T> OptionCell<T> {
    pub fn new(data: T) -> Self {
        OptionCell(Some(data))
    }

    pub fn take(&mut self) -> T {
        match self.0.take() {
            Some(d) => d,
            None => panic!("no data to take for OptionCell"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // Test struct that implements AsIoData
    struct TestIoData {
        io_data: IoData,
    }

    impl AsIoData for TestIoData {
        fn as_io_data(&self) -> &IoData {
            &self.io_data
        }
    }

    #[test]
    fn test_option_cell_new() {
        let cell = OptionCell::new(42);
        assert_eq!(*cell, 42);
    }

    #[test]
    fn test_option_cell_deref() {
        let cell = OptionCell::new(String::from("hello"));
        assert_eq!(cell.len(), 5);
        assert_eq!(cell.as_str(), "hello");
    }

    #[test]
    fn test_option_cell_take() {
        let mut cell = OptionCell::new(42);
        let value = cell.take();
        assert_eq!(value, 42);
    }

    #[test]
    #[should_panic(expected = "no data to take for OptionCell")]
    fn test_option_cell_take_panic() {
        let mut cell = OptionCell::new(42);
        let _value = cell.take();
        // Second take should panic
        let _value2 = cell.take();
    }

    #[test]
    #[should_panic(expected = "no data to deref for OptionCell")]
    fn test_option_cell_deref_panic() {
        let mut cell = OptionCell::new(42);
        let _value = cell.take();
        // Deref after take should panic
        let _deref = *cell;
    }

    #[test]
    fn test_option_cell_with_complex_type() {
        let data = vec![1, 2, 3, 4, 5];
        let cell = OptionCell::new(data);
        assert_eq!(cell.len(), 5);
        assert_eq!(cell[0], 1);
        assert_eq!(cell[4], 5);
    }

    #[test]
    fn test_option_cell_with_arc() {
        let data = Arc::new(42);
        let cell = OptionCell::new(data);
        assert_eq!(**cell, 42);
    }

    #[test]
    fn test_option_cell_take_with_string() {
        let mut cell = OptionCell::new(String::from("test"));
        let value = cell.take();
        assert_eq!(value, "test");
    }

    #[test]
    fn test_as_io_data_trait() {
        // Create a dummy socket for testing
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let io_data = IoData::new(&socket);
        let test_data = TestIoData { io_data };
        
        // Test that we can call as_io_data
        let io_ref = test_data.as_io_data();
        
        // Just verify we get a valid reference
        assert!(!std::ptr::eq(io_ref as *const _, std::ptr::null()));
    }

    #[test]
    fn test_option_cell_multiple_operations() {
        let cell = OptionCell::new(vec![1, 2, 3]);
        
        // Test multiple derefs
        assert_eq!(cell.len(), 3);
        assert_eq!(cell[0], 1);
        assert_eq!(cell[1], 2);
        assert_eq!(cell[2], 3);
        
        // Test that we can still deref after multiple operations
        assert_eq!(cell.len(), 3);
    }
}
