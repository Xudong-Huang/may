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
    // NOTE: field order matters. `io` must be dropped before `inner`, so the
    // fd is removed from the selector while it is still open. If `inner` were
    // dropped first, the fd number could be reused by another thread before
    // `IoData::drop` runs its `epoll_ctl(DEL)`; the DEL only knows the fd
    // number, so it would remove the registration of the new socket instead,
    // and that socket would never receive an io event again.
    io: io_impl::IoData,
    inner: T,
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

    /// Check whether `target` is registered in any epoll instance of this
    /// process by inspecting `/proc/self/fdinfo`.
    #[cfg(target_os = "linux")]
    fn fd_is_watched(target: RawFd) -> bool {
        let dir = match std::fs::read_dir("/proc/self/fd") {
            Ok(dir) => dir,
            Err(_) => return false,
        };
        for entry in dir.flatten() {
            let name = entry.file_name();
            let info = match std::fs::read_to_string(format!(
                "/proc/self/fdinfo/{}",
                name.to_string_lossy()
            )) {
                Ok(info) => info,
                Err(_) => continue,
            };
            let tokens: Vec<&str> = info.split_whitespace().collect();
            for pair in tokens.windows(2) {
                if pair[0] == "tfd:" && pair[1].parse::<RawFd>() == Ok(target) {
                    return true;
                }
            }
        }
        false
    }

    /// A raw fd owner whose drop replaces the file description at its fd
    /// number and then blocks, keeping the close/deregister window open.
    #[cfg(target_os = "linux")]
    struct ReplacingFd {
        stream: Option<std::os::unix::net::UnixStream>,
        replacement: Option<std::os::unix::net::UnixStream>,
        replaced_tx: std::sync::mpsc::Sender<()>,
        go_rx: std::sync::mpsc::Receiver<()>,
    }

    #[cfg(target_os = "linux")]
    impl AsRawFd for ReplacingFd {
        fn as_raw_fd(&self) -> RawFd {
            self.stream.as_ref().unwrap().as_raw_fd()
        }
    }

    #[cfg(target_os = "linux")]
    impl Drop for ReplacingFd {
        fn drop(&mut self) {
            let target = self.as_raw_fd();
            // give up ownership without closing the fd; dup2 replaces the
            // file description at this fd number, closing the old one
            let _ = self.stream.take().unwrap().into_raw_fd();
            let replacement = self.replacement.as_ref().unwrap().as_raw_fd();
            assert_eq!(unsafe { libc::dup2(replacement, target) }, target);
            self.replaced_tx.send(()).unwrap();
            // hold the window between closing the old description and the
            // IoData drop so that a new registration can be installed
            let _ = self.go_rx.recv_timeout(std::time::Duration::from_secs(10));
        }
    }

    /// Registering an fd that is already in the epoll set fails with
    /// `EEXIST`. The `IoData` built for the failed attempt must not run its
    /// `Drop` (which issues `epoll_ctl(DEL)`): that would remove the
    /// registration belonging to the live `CoIo`, and the socket would
    /// silently stop receiving io events.
    #[test]
    #[cfg(target_os = "linux")]
    fn failed_add_does_not_deregister_existing_fd() {
        use std::os::fd::AsFd;

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let fd = listener.as_raw_fd();
        let co = CoIo::new(listener).unwrap();
        assert!(fd_is_watched(fd), "original socket should be registered");

        // a second CoIo over the same fd makes `epoll_ctl(ADD)` fail
        let err = CoIo::new(co.inner().as_fd()).unwrap_err();
        assert_eq!(err.into_data().as_raw_fd(), fd);

        assert!(
            fd_is_watched(fd),
            "failed ADD deregistered the fd owned by the live CoIo"
        );
        drop(co);
    }

    /// Closing the socket before deregistering it from the selector lets a
    /// concurrent fd reuse install a new registration under the same fd
    /// number, which the stale `epoll_ctl(DEL)` then removes. The reused
    /// socket would then never receive an io event again.
    #[test]
    #[cfg(target_os = "linux")]
    fn drop_does_not_remove_reused_fd_registration() {
        use std::os::unix::io::FromRawFd;
        use std::sync::mpsc::channel;

        let (old_a, _old_b) = std::os::unix::net::UnixStream::pair().unwrap();
        let target = old_a.as_raw_fd();
        let (replacement, _replacement_peer) = std::os::unix::net::UnixStream::pair().unwrap();

        let (replaced_tx, replaced_rx) = channel();
        let (go_tx, go_rx) = channel();
        let (done_tx, done_rx) = channel();

        let old_co = CoIo::new(ReplacingFd {
            stream: Some(old_a),
            replacement: Some(replacement),
            replaced_tx,
            go_rx,
        })
        .unwrap();

        let drop_thread = std::thread::spawn(move || {
            drop(old_co);
            let _ = done_tx.send(());
        });

        // wait until the old description is gone and the fd number now
        // refers to the replacement socket
        replaced_rx.recv().unwrap();

        // register the replacement socket under the same fd number
        let replacement = unsafe { std::os::unix::net::UnixStream::from_raw_fd(target) };
        let replacement_co = CoIo::new(replacement).unwrap();
        assert!(
            fd_is_watched(target),
            "replacement socket should be registered"
        );

        // let the old CoIo finish dropping
        go_tx.send(()).unwrap();
        done_rx.recv().unwrap();
        drop_thread.join().unwrap();

        assert!(
            fd_is_watched(target),
            "dropping the old CoIo removed the epoll registration of a reused fd"
        );
        drop(replacement_co);
    }
}
