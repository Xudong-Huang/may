//! # Generic Wrapper for IO object
//! `CoIo` is a generic wrapper type that can be used in coroutine
//! context with non blocking operations
//!

use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle, IntoRawHandle, RawHandle};
use std::time::Duration;

use super::pipe::{PipeRead, PipeWrite};
use crate::io as io_impl;
use crate::sync::atomic_dur::AtomicDuration;
use crate::yield_now::yield_with;

/// Generic wrapper for any type that can be converted to raw `fd/HANDLE`
/// this type can be used in coroutine context without blocking the thread
#[derive(Debug)]
pub struct CoIo<T: AsRawHandle> {
    inner: T,
    io: io_impl::IoData,
    ctx: io_impl::IoContext,
    read_timeout: AtomicDuration,
    write_timeout: AtomicDuration,
}

impl<T: AsRawHandle> AsRawHandle for CoIo<T> {
    fn as_raw_handle(&self) -> RawHandle {
        self.inner.as_raw_handle()
    }
}

impl<T: AsRawHandle + IntoRawHandle> IntoRawHandle for CoIo<T> {
    fn into_raw_handle(self) -> RawHandle {
        self.inner.into_raw_handle()
    }
}

impl<T: AsRawHandle> CoIo<T> {
    /// create `CoIo` instance from `T`
    pub fn new(io: T) -> io::Result<Self> {
        use std::os::windows::io::{AsRawSocket, RawSocket};
        struct CoHandle(RawHandle);
        impl AsRawSocket for CoHandle {
            fn as_raw_socket(&self) -> RawSocket {
                self.0 as RawSocket
            }
        }

        let handle = CoHandle(io.as_raw_handle());
        io_impl::add_socket(&handle).map(|io_data| CoIo {
            inner: io,
            io: io_data,
            ctx: io_impl::IoContext::new(),
            read_timeout: AtomicDuration::new(None),
            write_timeout: AtomicDuration::new(None),
        })
    }

    /// reset internal io data
    #[allow(dead_code)]
    pub(crate) fn io_reset(&self) {
        self.io.reset()
    }

    /// check current ctx
    pub(crate) fn ctx_check(&self) -> io::Result<bool> {
        // FIXME: overlappened doesn't depend on the nonblocking?
        // self.ctx.check_nonblocking(|_| Ok(()))?;
        self.ctx.check_context(|_| Ok(()))
    }

    /// get inner ref
    pub fn inner(&self) -> &T {
        &self.inner
    }

    /// get inner mut ref
    pub fn inner_mut(&mut self) -> &T {
        &mut self.inner
    }

    /// convert back to original type
    pub fn into_inner(self) -> T {
        self.inner
    }

    /// get read timeout
    pub fn read_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(self.read_timeout.get())
    }

    /// get write timeout
    pub fn write_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(self.write_timeout.get())
    }

    /// set read timeout
    pub fn set_read_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        self.read_timeout.swap(dur);
        Ok(())
    }

    /// set write timeout
    pub fn set_write_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        self.write_timeout.swap(dur);
        Ok(())
    }

    /// set nonblocking
    pub fn set_nonblocking(&self, nb: bool) -> io::Result<()> {
        //set_nonblocking(self, nb)
        self.ctx.set_nonblocking(nb);
        Ok(())
    }
}

impl<T: AsRawHandle + Read> Read for CoIo<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.ctx_check()? {
            return self.inner.read(buf);
        }

        self.io.reset();
        let reader = PipeRead::new(self, buf, self.read_timeout.get());
        yield_with(&reader);
        reader.done()
    }
}

impl<T: AsRawHandle + Write> Write for CoIo<T> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.ctx_check()? {
            return self.inner.write(buf);
        }

        self.io.reset();
        let writer = PipeWrite::new(self, buf, self.write_timeout.get());
        yield_with(&writer);
        writer.done()
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

// impl<'a, T: AsRawHandle + Read> Read for &'a CoIo<T> {
//     fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
//         let s = unsafe { &mut *(*self as *const _ as *mut _) };
//         CoIo::<T>::read(s, buf)
//     }
// }

// impl<'a, T: AsRawHandle + Write> Write for &'a CoIo<T> {
//     fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
//         let s = unsafe { &mut *(*self as *const _ as *mut _) };
//         CoIo::<T>::write(s, buf)
//     }

//     fn flush(&mut self) -> io::Result<()> {
//         let s = unsafe { &mut *(*self as *const _ as *mut _) };
//         CoIo::<T>::flush(s)
//     }
// }

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(dead_code)]
    fn compile_co_io() {
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
        let mut io = CoIo::new(a).unwrap();
        let mut buf = [0u8; 100];
        io.read(&mut buf).unwrap();
    }
}
