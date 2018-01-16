//! # Generic Wrapper for IO object
//! `CoIo` is a generic wrapper type that can be used in coroutine
//! context with non blocking operations
//!

use libc;
use io as io_impl;
use yield_now::yield_with;
use self::io_impl::net as net_impl;
use self::io_impl::co_io_err::Error;

use std::time::Duration;
use std::io::{self, Read, Write};
use std::os::unix::io::{AsRawFd, IntoRawFd, RawFd};

fn set_nonblocking<T: AsRawFd>(fd: &T, nb: bool) -> io::Result<()> {
    unsafe {
        let fd = fd.as_raw_fd();
        let r = libc::fcntl(fd, libc::F_GETFL);
        if r == -1 {
            return Err(io::Error::last_os_error());
        }

        let r = if nb {
            libc::fcntl(fd, libc::F_SETFL, r | libc::O_NONBLOCK)
        } else {
            libc::fcntl(fd, libc::F_SETFL, r & !libc::O_NONBLOCK)
        };

        if r == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// Generic wrapper for any type that can be converted to raw `fd/HANDLE`
/// this type can be used in coroutine context without blocking the thread
#[derive(Debug)]
pub struct CoIo<T: AsRawFd> {
    inner: T,
    io: io_impl::IoData,
    ctx: io_impl::IoContext,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
}

impl<T: AsRawFd> io_impl::AsIoData for CoIo<T> {
    fn as_io_data(&self) -> &io_impl::IoData {
        &self.io
    }
}

impl<T: AsRawFd> AsRawFd for CoIo<T> {
    fn as_raw_fd(&self) -> RawFd {
        self.inner.as_raw_fd()
    }
}

impl<T: AsRawFd + IntoRawFd> IntoRawFd for CoIo<T> {
    fn into_raw_fd(self) -> RawFd {
        self.inner.into_raw_fd()
    }
}

impl<T: AsRawFd> CoIo<T> {
    /// create `CoIo` instance from `T`
    pub fn new(io: T) -> Result<Self, Error<T>> {
        let io_data = match io_impl::add_socket(&io) {
            Ok(o) => o,
            Err(e) => return Err(Error::new(e, io)),
        };

        match set_nonblocking(&io, true) {
            Ok(_) => {}
            Err(e) => return Err(Error::new(e, io)),
        }

        Ok(CoIo {
            inner: io,
            io: io_data,
            ctx: io_impl::IoContext::new(),
            read_timeout: None,
            write_timeout: None,
        })
    }

    /// create from raw io ojbect which is already registered
    pub(crate) fn from_raw(io: T, io_data: io_impl::IoData) -> Self {
        CoIo {
            inner: io,
            io: io_data,
            ctx: io_impl::IoContext::new(),
            read_timeout: None,
            write_timeout: None,
        }
    }

    /// reset internal io data
    pub(crate) fn io_reset(&self) {
        self.io.reset()
    }

    /// check current ctx
    pub(crate) fn ctx_check(&self) -> io::Result<bool> {
        self.ctx.check(|| self.set_nonblocking(false))
    }

    fn set_nonblocking(&self, nb: bool) -> io::Result<()> {
        set_nonblocking(self, nb)
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

impl<T: AsRawFd + Read> Read for CoIo<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.ctx_check()? {
            // this can't be nonblocking!!
            return self.inner.read(buf);
        }

        self.io.reset();
        // this is an earlier return try for nonblocking read
        // it's useful for server but not necessary for client
        match self.inner.read(buf) {
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
            ret => return ret,
        }

        let reader = net_impl::SocketRead::new(self, buf, self.read_timeout);
        yield_with(&reader);
        reader.done()
    }
}

impl<T: AsRawFd + Write> Write for CoIo<T> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.ctx_check()? {
            // this can't be nonblocking!!
            return self.inner.write(buf);
        }

        self.io.reset();
        // this is an earlier return try for nonblocking write
        match self.inner.write(buf) {
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
            ret => return ret,
        }

        let writer = net_impl::SocketWrite::new(self, buf, self.write_timeout);
        yield_with(&writer);
        writer.done()
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<'a, T: AsRawFd + Read> Read for &'a CoIo<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let s = unsafe { &mut *(*self as *const _ as *mut _) };
        CoIo::<T>::read(s, buf)
    }
}

impl<'a, T: AsRawFd + Write> Write for &'a CoIo<T> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let s = unsafe { &mut *(*self as *const _ as *mut _) };
        CoIo::<T>::write(s, buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        let s = unsafe { &mut *(*self as *const _ as *mut _) };
        CoIo::<T>::flush(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(dead_code)]
    fn compile_co_io() {
        use std::os::unix::io::RawFd;

        #[derive(Debug)]
        struct Fd;

        impl AsRawFd for Fd {
            fn as_raw_fd(&self) -> RawFd {
                0
            }
        }

        impl Read for Fd {
            fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
                Ok(0)
            }
        }

        let a = Fd;
        let mut io = CoIo::new(a).unwrap();
        let mut buf = [0u8; 100];
        io.read(&mut buf).unwrap();
    }
}
