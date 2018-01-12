//! # Generic Wrapper for IO object
//! `CoIO` is a generic wrapper type that can be used in coroutine
//! context with non blocking operations
//!

use io as io_impl;
use yield_now::yield_with;
use self::io_impl::net as net_impl;

use std::time::Duration;
use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle, AsRawSocket, RawSocket};

/// Wrap `AsRawHanle` type to `AsRawSocket` so that we can use it in `CoIO`
/// e.g. `let io = CoIO::new(CoHandle(T))`
#[derive(Debug)]
pub struct CoHandle<T: AsRawHandle>(pub T);

impl<T: AsRawHandle> AsRawSocket for CoHandle<T> {
    fn as_raw_socket(&self) -> RawSocket {
        self.0.as_raw_handle() as RawSocket
    }
}

impl<T: AsRawHandle + Read> Read for CoHandle<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl<T: AsRawHandle + Write> Write for CoHandle<T> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

/// Generic wrapper for any type that can be converted to raw `fd/HANDLE`
/// this type can be used in coroutine context without blocking the thread
#[derive(Debug)]
pub struct CoIO<T: AsRawSocket> {
    inner: T,
    io: io_impl::IoData,
    ctx: io_impl::IoContext,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
}

impl<T: AsRawSocket> AsRawSocket for CoIO<T> {
    fn as_raw_socket(&self) -> RawSocket {
        self.inner.as_raw_socket()
    }
}

impl<T: AsRawSocket> CoIO<T> {
    /// create `CoIO` instance from `T`
    pub fn new(io: T) -> io::Result<Self> {
        io_impl::add_socket(&io).map(|io_data| CoIO {
            inner: io,
            io: io_data,
            ctx: io_impl::IoContext::new(),
            read_timeout: None,
            write_timeout: None,
        })
    }

    /// convert back to original type
    pub fn into_raw(self) -> T {
        self.inner
    }

    /// get read timeout
    pub fn read_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(self.read_timeout)
    }

    /// get write timeout
    pub fn write_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(self.write_timeout)
    }

    /// set read timeout
    pub fn set_read_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        let me = unsafe { &mut *(self as *const _ as *mut Self) };
        me.read_timeout = dur;
        Ok(())
    }

    /// set write timeout
    pub fn set_write_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        let me = unsafe { &mut *(self as *const _ as *mut Self) };
        me.write_timeout = dur;
        Ok(())
    }
}

impl<T: AsRawSocket + Read> Read for CoIO<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.io.reset();
        let reader = net_impl::SocketRead::new(self, buf, self.read_timeout);
        yield_with(&reader);
        reader.done()
    }
}

impl<T: AsRawSocket + Write> Write for CoIO<T> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.io.reset();
        let writer = net_impl::SocketWrite::new(self, buf, self.write_timeout);
        yield_with(&writer);
        writer.done()
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<'a, T: AsRawSocket + Read> Read for &'a CoIO<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let s = unsafe { &mut *(*self as *const _ as *mut _) };
        CoIO::<T>::read(s, buf)
    }
}

impl<'a, T: AsRawSocket + Write> Write for &'a CoIO<T> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let s = unsafe { &mut *(*self as *const _ as *mut _) };
        CoIO::<T>::write(s, buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        let s = unsafe { &mut *(*self as *const _ as *mut _) };
        CoIO::<T>::flush(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(dead_code)]
    fn compile_co_io() {
        use std::os::windows::io::RawHandle;

        #[derive(Debug)]
        struct Hd;

        impl AsRawHandle for Hd {
            fn as_raw_handle(&self) -> RawHandle {
                0 as _
            }
        }

        impl Read for Hd {
            fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
                Ok(0)
            }
        }

        let a = Hd;
        let mut io = CoIO::new(CoHandle(a)).unwrap();
        let mut buf = [0u8; 100];
        io.read(&mut buf).unwrap();
    }
}
