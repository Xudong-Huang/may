//! # Generic Wrapper for IO object
//! `CoIO` is a generic wrapper type that can be used in coroutine
//! context with non blocking operations
//!

use io as io_impl;
use yield_now::yield_with;
use self::io_impl::net as net_impl;

use std::time::Duration;
use std::io::{self, Read, Write};
use std::os::unix::io::{AsRawFd, RawFd};

fn set_nonblocking<T: AsRawFd>(fd: &T, nb: bool) -> io::Result<()> {
    use libc;
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

pub struct CoIO<T: AsRawFd> {
    inner: T,
    io: io_impl::IoData,
    ctx: io_impl::IoContext,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
}

impl<T: AsRawFd> io_impl::AsIoData for CoIO<T> {
    fn as_io_data(&self) -> &io_impl::IoData {
        &self.io
    }
}

impl<T: AsRawFd> AsRawFd for CoIO<T> {
    fn as_raw_fd(&self) -> RawFd {
        self.inner.as_raw_fd()
    }
}

impl<T: AsRawFd> CoIO<T> {
    /// create `CoIO` instance from `T`
    pub fn new(io: T) -> io::Result<Self> {
        // only set non blocking in coroutine context
        // we would first call nonblocking io in the coroutine
        // to avoid unnecessary context switch
        set_nonblocking(&io, true)?;

        io_impl::add_socket(&io).map(|io_data| CoIO {
            inner: io,
            io: io_data,
            ctx: io_impl::IoContext::new(),
            read_timeout: None,
            write_timeout: None,
        })
    }

    fn set_nonblocking(&self, nb: bool) -> io::Result<()> {
        set_nonblocking(self, nb)
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

impl<T: AsRawFd + Read> Read for CoIO<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.ctx.check(|| self.set_nonblocking(false))? {
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

impl<T: AsRawFd + Write> Write for CoIO<T> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.ctx.check(|| self.set_nonblocking(false))? {
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

impl<'a, T: AsRawFd + Read> Read for &'a CoIO<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let s = unsafe { &mut *(*self as *const _ as *mut _) };
        CoIO::<T>::read(s, buf)
    }
}

impl<'a, T: AsRawFd + Write> Write for &'a CoIO<T> {
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
        let mut io = CoIO::new(a).unwrap();
        let mut buf = [0u8; 100];
        io.read(&mut buf).unwrap();
    }
}
