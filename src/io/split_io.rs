//! Split io object into read/write part
//!

use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::fd::{AsRawFd, RawFd};

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
