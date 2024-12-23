//! # Generic Wrapper for IO object
//! `CoIo` is a generic wrapper type that can be used in coroutine
//! context with non blocking operations
//!

use std::io::{self, Read, Write};
use std::os::unix::io::{AsRawFd, IntoRawFd, RawFd};
#[cfg(feature = "io_timeout")]
use std::time::Duration;

use self::io_impl::co_io_err::Error;
use self::io_impl::net as net_impl;
use super::from_nix_error;
use crate::io as io_impl;
#[cfg(feature = "io_timeout")]
use crate::sync::atomic_dur::AtomicDuration;
use crate::yield_now::yield_with_io;

use nix::sys::socket::{recv, MsgFlags};

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
    #[cfg(feature = "io_timeout")]
    read_timeout: AtomicDuration,
    #[cfg(feature = "io_timeout")]
    write_timeout: AtomicDuration,
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
            #[cfg(feature = "io_timeout")]
            read_timeout: AtomicDuration::new(None),
            #[cfg(feature = "io_timeout")]
            write_timeout: AtomicDuration::new(None),
        })
    }

    /// create from raw io object which is already registered
    pub(crate) fn from_raw(io: T, io_data: io_impl::IoData) -> Self {
        CoIo {
            inner: io,
            io: io_data,
            #[cfg(feature = "io_timeout")]
            read_timeout: AtomicDuration::new(None),
            #[cfg(feature = "io_timeout")]
            write_timeout: AtomicDuration::new(None),
        }
    }

    /// reset internal io data
    pub(crate) fn io_reset(&self) {
        self.io.reset();
    }

    /// get inner ref
    #[inline]
    pub fn inner(&self) -> &T {
        &self.inner
    }

    /// get inner mut ref
    #[inline]
    pub fn inner_mut(&mut self) -> &mut T {
        &mut self.inner
    }

    /// convert back to original type
    pub fn into_inner(self) -> T {
        self.inner
    }

    /// get read timeout
    #[cfg(feature = "io_timeout")]
    pub fn read_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(self.read_timeout.get())
    }

    /// get write timeout
    #[cfg(feature = "io_timeout")]
    pub fn write_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(self.write_timeout.get())
    }

    /// set read timeout
    #[cfg(feature = "io_timeout")]
    pub fn set_read_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        self.read_timeout.store(dur);
        Ok(())
    }

    /// set write timeout
    #[cfg(feature = "io_timeout")]
    pub fn set_write_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        self.write_timeout.store(dur);
        Ok(())
    }

    /// Receives data on the socket from the remote address to which it is
    /// connected, without removing that data from the queue. On success,
    /// returns the number of bytes peeked.
    pub fn peek(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.io.reset();
        // this is an earlier return try for nonblocking read
        // it's useful for server but not necessary for client
        match recv(self.io.fd, buf, MsgFlags::MSG_PEEK) {
            Ok(n) => return Ok(n),
            Err(e) => {
                if e == nix::errno::Errno::EAGAIN {
                    // do nothing
                } else {
                    return Err(from_nix_error(e));
                }
            }
        }

        let mut reader = net_impl::SocketPeek::new(
            self,
            buf,
            #[cfg(feature = "io_timeout")]
            self.read_timeout.get(),
        );
        yield_with_io(&reader, reader.is_coroutine);
        reader.done()
    }
}

impl<T: AsRawFd + Read> Read for CoIo<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.io.reset();
        // this is an earlier return try for nonblocking read
        // it's useful for server but not necessary for client
        match self.inner.read(buf) {
            Ok(n) => return Ok(n),
            Err(e) => {
                // raw_os_error is faster than kind
                let raw_err = e.raw_os_error();
                if raw_err == Some(libc::EAGAIN) || raw_err == Some(libc::EWOULDBLOCK) {
                    // do nothing here
                } else {
                    return Err(e);
                }
            }
        }

        let mut reader = net_impl::SocketRead::new(
            self,
            buf,
            #[cfg(feature = "io_timeout")]
            self.read_timeout.get(),
        );
        yield_with_io(&reader, reader.is_coroutine);
        reader.done()
    }
}

impl<T: AsRawFd + Write> Write for CoIo<T> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.io.reset();
        // this is an earlier return try for nonblocking write
        match self.inner.write(buf) {
            Ok(n) => return Ok(n),
            Err(e) => {
                // raw_os_error is faster than kind
                let raw_err = e.raw_os_error();
                if raw_err == Some(libc::EAGAIN) || raw_err == Some(libc::EWOULDBLOCK) {
                    // do nothing here
                } else {
                    return Err(e);
                }
            }
        }

        let mut writer = net_impl::SocketWrite::new(
            self,
            buf,
            #[cfg(feature = "io_timeout")]
            self.write_timeout.get(),
        );
        yield_with_io(&writer, writer.is_coroutine);
        writer.done()
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

// impl<'a, T: AsRawFd + Read> Read for &'a CoIo<T> {
//     fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
//         let s = unsafe { &mut *(*self as *const _ as *mut _) };
//         CoIo::<T>::read(s, buf)
//     }
// }

// impl<'a, T: AsRawFd + Write> Write for &'a CoIo<T> {
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

    #[test]
    fn compile_co_io() {
        #[derive(Debug)]
        struct Fd {
            file: std::net::UdpSocket,
        }

        impl Fd {
            fn new() -> Self {
                Fd {
                    // this would call set_nonblocking for the fd
                    // so we need to open a real fd here
                    file: std::net::UdpSocket::bind(("127.0.0.1", 9765)).unwrap(),
                }
            }
        }

        impl AsRawFd for Fd {
            fn as_raw_fd(&self) -> RawFd {
                self.file.as_raw_fd()
            }
        }

        impl Read for Fd {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                buf.fill(0x55);
                Ok(buf.len())
            }
        }

        let a = Fd::new();
        let mut io = CoIo::new(a).unwrap();
        let mut buf = [0u8; 100];
        io.read_exact(&mut buf).unwrap();
        assert_eq!(buf, [0x55u8; 100]);
    }
}
