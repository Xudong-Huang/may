//! Split io object into read/write part
//!

use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::fd::{AsRawFd, RawFd};
#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, RawHandle};

use super::AsIoData;

pub struct SplitReader<T> {
    inner: T,
}

impl<T> SplitReader<T> {
    pub(crate) fn new(io: T) -> Self {
        SplitReader { inner: io }
    }

    pub fn inner(&self) -> &T {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}

pub struct SplitWriter<T> {
    inner: T,
}

impl<T> SplitWriter<T> {
    pub(crate) fn new(io: T) -> Self {
        SplitWriter { inner: io }
    }

    pub fn inner(&self) -> &T {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}

impl<T> AsIoData for SplitWriter<T>
where
    T: AsIoData,
{
    fn as_io_data(&self) -> &super::IoData {
        self.inner.as_io_data()
    }
}

impl<T> AsIoData for SplitReader<T>
where
    T: AsIoData,
{
    fn as_io_data(&self) -> &super::IoData {
        self.inner.as_io_data()
    }
}

#[cfg(unix)]
impl<T> AsRawFd for SplitReader<T>
where
    T: AsRawFd,
{
    fn as_raw_fd(&self) -> RawFd {
        self.inner.as_raw_fd()
    }
}

#[cfg(unix)]
impl<T> AsRawFd for SplitWriter<T>
where
    T: AsRawFd,
{
    fn as_raw_fd(&self) -> RawFd {
        self.inner.as_raw_fd()
    }
}

#[cfg(windows)]
impl<T> AsRawHandle for SplitReader<T>
where
    T: AsRawHandle,
{
    fn as_raw_handle(&self) -> RawHandle {
        self.inner.as_raw_handle()
    }
}

#[cfg(windows)]
impl<T> AsRawHandle for SplitWriter<T>
where
    T: AsRawHandle,
{
    fn as_raw_handle(&self) -> RawHandle {
        self.inner.as_raw_handle()
    }
}

impl<T: Read> Read for SplitReader<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl<T: Write> Write for SplitWriter<T> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// This is trait that split an io obj into two parts
/// one is for read operation, another is for write operation
pub trait SplitIo {
    /// split the io into read and write part
    fn split(self) -> io::Result<(SplitReader<Self>, SplitWriter<Self>)>
    where
        Self: Read + Write + Sized;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Read, Write};

    // Mock IO type for testing
    struct MockIo {
        data: Vec<u8>,
        read_pos: usize,
    }

    impl MockIo {
        fn new(data: Vec<u8>) -> Self {
            MockIo { data, read_pos: 0 }
        }
    }

    impl Read for MockIo {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let remaining = self.data.len() - self.read_pos;
            let to_read = buf.len().min(remaining);

            if to_read > 0 {
                buf[..to_read].copy_from_slice(&self.data[self.read_pos..self.read_pos + to_read]);
                self.read_pos += to_read;
            }

            Ok(to_read)
        }
    }

    impl Write for MockIo {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.data.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    // Mock AsIoData implementation
    use crate::io::IoData;

    impl AsIoData for MockIo {
        fn as_io_data(&self) -> &IoData {
            // For testing purposes, we'll use a dummy file descriptor
            #[cfg(unix)]
            {
                use std::os::fd::AsRawFd;
                struct DummyFd;
                impl AsRawFd for DummyFd {
                    fn as_raw_fd(&self) -> std::os::fd::RawFd {
                        0 // stdin as a dummy fd
                    }
                }
                static DUMMY_FD: DummyFd = DummyFd;
                static IO_DATA: std::sync::OnceLock<IoData> = std::sync::OnceLock::new();
                IO_DATA.get_or_init(|| IoData::new(&DUMMY_FD))
            }
            #[cfg(windows)]
            {
                use std::os::windows::io::AsRawHandle;
                struct DummyHandle;
                impl AsRawHandle for DummyHandle {
                    fn as_raw_handle(&self) -> std::os::windows::io::RawHandle {
                        std::ptr::null_mut() // dummy handle
                    }
                }
                static DUMMY_HANDLE: DummyHandle = DummyHandle;
                static IO_DATA: std::sync::OnceLock<IoData> = std::sync::OnceLock::new();
                IO_DATA.get_or_init(|| IoData::new(&DUMMY_HANDLE))
            }
        }
    }

    #[test]
    fn test_split_reader_new() {
        let mock_io = MockIo::new(vec![1, 2, 3, 4, 5]);
        let reader = SplitReader::new(mock_io);
        assert_eq!(reader.inner().data, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn test_split_reader_inner_access() {
        let mock_io = MockIo::new(vec![1, 2, 3]);
        let reader = SplitReader::new(mock_io);

        // Test inner() method
        assert_eq!(reader.inner().data, vec![1, 2, 3]);

        // Test inner_mut() method
        let mut reader = reader;
        reader.inner_mut().data.push(4);
        assert_eq!(reader.inner().data, vec![1, 2, 3, 4]);
    }

    #[test]
    fn test_split_reader_read() {
        let mock_io = MockIo::new(vec![1, 2, 3, 4, 5]);
        let mut reader = SplitReader::new(mock_io);

        let mut buf = [0u8; 3];
        let bytes_read = reader.read(&mut buf).unwrap();
        assert_eq!(bytes_read, 3);
        assert_eq!(buf, [1, 2, 3]);

        // Read remaining bytes
        let mut buf = [0u8; 5];
        let bytes_read = reader.read(&mut buf).unwrap();
        assert_eq!(bytes_read, 2);
        assert_eq!(buf[..2], [4, 5]);
    }

    #[test]
    fn test_split_writer_new() {
        let mock_io = MockIo::new(vec![]);
        let writer = SplitWriter::new(mock_io);
        assert_eq!(writer.inner().data, vec![]);
    }

    #[test]
    fn test_split_writer_inner_access() {
        let mock_io = MockIo::new(vec![1, 2]);
        let writer = SplitWriter::new(mock_io);

        // Test inner() method
        assert_eq!(writer.inner().data, vec![1, 2]);

        // Test inner_mut() method
        let mut writer = writer;
        writer.inner_mut().data.push(3);
        assert_eq!(writer.inner().data, vec![1, 2, 3]);
    }

    #[test]
    fn test_split_writer_write() {
        let mock_io = MockIo::new(vec![]);
        let mut writer = SplitWriter::new(mock_io);

        let data = b"hello world";
        let bytes_written = writer.write(data).unwrap();
        assert_eq!(bytes_written, data.len());
        assert_eq!(writer.inner().data, data);

        // Test flush
        writer.flush().unwrap();
    }

    #[test]
    fn test_split_reader_as_io_data() {
        let mock_io = MockIo::new(vec![1, 2, 3]);
        let reader = SplitReader::new(mock_io);

        // Test AsIoData trait implementation
        let _io_data = reader.as_io_data();
        // Just verify it doesn't panic and returns something
    }

    #[test]
    fn test_split_writer_as_io_data() {
        let mock_io = MockIo::new(vec![]);
        let writer = SplitWriter::new(mock_io);

        // Test AsIoData trait implementation
        let _io_data = writer.as_io_data();
        // Just verify it doesn't panic and returns something
    }

    #[cfg(unix)]
    #[test]
    fn test_split_reader_as_raw_fd() {
        use std::os::fd::AsRawFd;

        // Create a mock that implements AsRawFd
        struct MockFd;
        impl AsRawFd for MockFd {
            fn as_raw_fd(&self) -> std::os::fd::RawFd {
                42 // Mock file descriptor
            }
        }
        impl Read for MockFd {
            fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
                Ok(0)
            }
        }
        impl AsIoData for MockFd {
            fn as_io_data(&self) -> &IoData {
                static IO_DATA: std::sync::OnceLock<IoData> = std::sync::OnceLock::new();
                IO_DATA.get_or_init(|| IoData::new(self))
            }
        }

        let mock_fd = MockFd;
        let reader = SplitReader::new(mock_fd);
        assert_eq!(reader.as_raw_fd(), 42);
    }

    #[cfg(unix)]
    #[test]
    fn test_split_writer_as_raw_fd() {
        use std::os::fd::AsRawFd;

        // Create a mock that implements AsRawFd
        struct MockFd;
        impl AsRawFd for MockFd {
            fn as_raw_fd(&self) -> std::os::fd::RawFd {
                24 // Mock file descriptor
            }
        }
        impl Write for MockFd {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        impl AsIoData for MockFd {
            fn as_io_data(&self) -> &IoData {
                static IO_DATA: std::sync::OnceLock<IoData> = std::sync::OnceLock::new();
                IO_DATA.get_or_init(|| IoData::new(self))
            }
        }

        let mock_fd = MockFd;
        let writer = SplitWriter::new(mock_fd);
        assert_eq!(writer.as_raw_fd(), 24);
    }

    #[test]
    fn test_split_io_trait_usage() {
        // Test that the SplitIo trait can be used
        // We'll create a simple implementation for testing
        struct TestIo {
            data: Vec<u8>,
            pos: usize,
        }

        impl TestIo {
            fn new() -> Self {
                TestIo {
                    data: Vec::new(),
                    pos: 0,
                }
            }
        }

        impl Read for TestIo {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                let available = self.data.len() - self.pos;
                let to_read = buf.len().min(available);

                if to_read > 0 {
                    buf[..to_read].copy_from_slice(&self.data[self.pos..self.pos + to_read]);
                    self.pos += to_read;
                }

                Ok(to_read)
            }
        }

        impl Write for TestIo {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.data.extend_from_slice(buf);
                Ok(buf.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        impl SplitIo for TestIo {
            fn split(self) -> io::Result<(SplitReader<Self>, SplitWriter<Self>)> {
                // Note: This is a simplified test implementation
                // In a real implementation, you would need to handle shared state properly
                // For testing purposes, we'll just verify the trait compiles
                let reader_io = TestIo::new();
                let writer_io = TestIo::new();
                Ok((SplitReader::new(reader_io), SplitWriter::new(writer_io)))
            }
        }

        // This test just verifies the trait compiles and can be used
        // In practice, the split would need to handle shared state properly
        let test_io = TestIo::new();
        let _result = test_io.split();
        // Just verify it compiles and doesn't panic
    }
}
