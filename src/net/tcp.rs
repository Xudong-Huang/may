use std::io::{self, Read, Write};
use std::net::{self, Shutdown, SocketAddr, ToSocketAddrs};
#[cfg(feature = "io_timeout")]
use std::time::Duration;

use crate::io as io_impl;
use crate::io::net as net_impl;
use crate::io::split_io::{SplitIo, SplitReader, SplitWriter};
#[cfg(unix)]
use crate::io::sys::mod_socket;
#[cfg(unix)]
use crate::io::AsIoData;
#[cfg(feature = "io_timeout")]
use crate::sync::atomic_dur::AtomicDuration;
use crate::yield_now::yield_with_io;

// ===== TcpStream =====
//
//

#[derive(Debug)]
pub struct TcpStream {
    _io: io_impl::IoData,
    sys: net::TcpStream,
    #[cfg(feature = "io_timeout")]
    read_timeout: AtomicDuration,
    #[cfg(feature = "io_timeout")]
    write_timeout: AtomicDuration,
}

impl TcpStream {
    fn new(s: net::TcpStream) -> io::Result<TcpStream> {
        // only set non blocking in coroutine context
        // we would first call nonblocking io in the coroutine
        // to avoid unnecessary context switch
        s.set_nonblocking(true)?;

        io_impl::add_socket(&s).map(|io| TcpStream {
            _io: io,
            sys: s,
            #[cfg(feature = "io_timeout")]
            read_timeout: AtomicDuration::new(None),
            #[cfg(feature = "io_timeout")]
            write_timeout: AtomicDuration::new(None),
        })
    }

    #[inline]
    pub fn inner(&self) -> &net::TcpStream {
        &self.sys
    }

    #[inline]
    pub fn inner_mut(&mut self) -> &mut net::TcpStream {
        &mut self.sys
    }

    pub fn connect<A: ToSocketAddrs>(addr: A) -> io::Result<TcpStream> {
        let mut c = net_impl::TcpStreamConnect::new(
            addr,
            #[cfg(feature = "io_timeout")]
            None,
        )?;

        #[cfg(unix)]
        {
            if c.check_connected()? {
                return c.done();
            }
        }

        yield_with_io(&c, c.is_coroutine);
        c.done()
    }

    #[cfg(feature = "io_timeout")]
    pub fn connect_timeout(addr: &SocketAddr, timeout: Duration) -> io::Result<TcpStream> {
        let mut c = net_impl::TcpStreamConnect::new(addr, Some(timeout))?;

        #[cfg(unix)]
        {
            if c.check_connected()? {
                return c.done();
            }
        }

        yield_with_io(&c, c.is_coroutine);
        c.done()
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.sys.peer_addr()
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.sys.local_addr()
    }

    #[cfg(not(windows))]
    pub fn try_clone(&self) -> io::Result<TcpStream> {
        let s = self.sys.try_clone().and_then(TcpStream::new)?;
        #[cfg(feature = "io_timeout")]
        s.set_read_timeout(self.read_timeout.get()).unwrap();
        #[cfg(feature = "io_timeout")]
        s.set_write_timeout(self.write_timeout.get()).unwrap();
        Ok(s)
    }

    // windows doesn't support add dup handler to IOCP
    #[cfg(windows)]
    pub fn try_clone(&self) -> io::Result<TcpStream> {
        let s = self.sys.try_clone()?;
        s.set_nonblocking(true)?;
        // ignore the result here
        // it always failed with "The parameter is incorrect"
        io_impl::add_socket(&s).ok();
        Ok(TcpStream {
            _io: io_impl::IoData::new(0),
            sys: s,
            #[cfg(feature = "io_timeout")]
            read_timeout: AtomicDuration::new(self.read_timeout.get()),
            #[cfg(feature = "io_timeout")]
            write_timeout: AtomicDuration::new(self.write_timeout.get()),
        })
    }

    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        self.sys.shutdown(how)
    }

    pub fn set_nodelay(&self, nodelay: bool) -> io::Result<()> {
        self.sys.set_nodelay(nodelay)
    }

    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        self.sys.take_error()
    }

    #[cfg(feature = "io_timeout")]
    pub fn set_read_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        self.sys.set_read_timeout(dur)?;
        self.read_timeout.store(dur);
        Ok(())
    }

    #[cfg(feature = "io_timeout")]
    pub fn set_write_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        self.sys.set_write_timeout(dur)?;
        self.write_timeout.store(dur);
        Ok(())
    }

    #[cfg(feature = "io_timeout")]
    pub fn read_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(self.read_timeout.get())
    }

    #[cfg(feature = "io_timeout")]
    pub fn write_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(self.write_timeout.get())
    }

    pub fn set_ttl(&self, ttl: u32) -> io::Result<()> {
        self.sys.set_ttl(ttl)
    }

    pub fn ttl(&self) -> io::Result<u32> {
        self.sys.ttl()
    }

    // convert std::net::TcpStream to Self without add_socket
    pub(crate) fn from_stream(s: net::TcpStream, io: io_impl::IoData) -> Self {
        TcpStream {
            _io: io,
            sys: s,
            #[cfg(feature = "io_timeout")]
            read_timeout: AtomicDuration::new(None),
            #[cfg(feature = "io_timeout")]
            write_timeout: AtomicDuration::new(None),
        }
    }
}

impl Read for TcpStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        #[cfg(unix)]
        {
            self._io.reset();
            // this is an earlier return try for nonblocking read
            // it's useful for server but not necessary for client
            match self.sys.read(buf) {
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

impl Write for TcpStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        #[cfg(unix)]
        {
            self._io.reset();
            // this is an earlier return try for nonblocking write
            match self.sys.write(buf) {
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

    #[cfg(unix)]
    fn write_vectored(&mut self, bufs: &[io::IoSlice<'_>]) -> io::Result<usize> {
        #[cfg(unix)]
        {
            self._io.reset();
            // this is an earlier return try for nonblocking write
            match self.sys.write_vectored(bufs) {
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
        }

        let mut writer = net_impl::SocketWriteVectored::new(
            self,
            &self.sys,
            bufs,
            #[cfg(feature = "io_timeout")]
            self.write_timeout.get(),
        );
        yield_with_io(&writer, writer.is_coroutine);
        writer.done()
    }

    fn flush(&mut self) -> io::Result<()> {
        // TcpStream just return Ok(()), no need to yield
        self.sys.flush()
    }
}

// impl<'a> Read for &'a TcpStream {
//     fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
//         let s = unsafe { &mut *(*self as *const _ as *mut _) };
//         TcpStream::read(s, buf)
//     }
// }

// impl<'a> Write for &'a TcpStream {
//     fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
//         let s = unsafe { &mut *(*self as *const _ as *mut _) };
//         TcpStream::write(s, buf)
//     }

//     fn flush(&mut self) -> io::Result<()> {
//         let s = unsafe { &mut *(*self as *const _ as *mut _) };
//         TcpStream::flush(s)
//     }
// }

#[cfg(unix)]
impl io_impl::AsIoData for TcpStream {
    fn as_io_data(&self) -> &io_impl::IoData {
        &self._io
    }
}

// ===== TcpListener =====
//
//

#[derive(Debug)]
pub struct TcpListener {
    _io: io_impl::IoData,
    sys: net::TcpListener,
}

impl TcpListener {
    fn new(s: net::TcpListener) -> io::Result<TcpListener> {
        // only set non blocking in coroutine context
        // we would first call nonblocking io in the coroutine
        // to avoid unnecessary context switch
        s.set_nonblocking(true)?;

        io_impl::add_socket(&s).map(|io| TcpListener { _io: io, sys: s })
    }

    #[inline]
    pub fn inner(&self) -> &net::TcpListener {
        &self.sys
    }

    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<TcpListener> {
        use socket2::{Domain, Socket, Type};
        let mut addrs = addr.to_socket_addrs()?;
        let addr = addrs.next().unwrap();
        let listener = match &addr {
            SocketAddr::V4(_) => Socket::new(Domain::IPV4, Type::STREAM, None)?,
            SocketAddr::V6(_) => Socket::new(Domain::IPV6, Type::STREAM, None)?,
        };

        // windows not have reuse port but reuse address is not safe
        listener.set_reuse_address(true)?;

        // #[cfg(unix)]
        // listener.set_reuse_port(true)?;

        listener.bind(&addr.into())?;
        // for addr in addrs {
        //     listener.bind(&addr.into())?;
        // }
        listener.listen(1024)?;

        let s = listener.into();
        TcpListener::new(s)
    }

    pub fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        #[cfg(unix)]
        {
            self._io.reset();
            match self.sys.accept() {
                Ok((s, a)) => return TcpStream::new(s).map(|s| (s, a)),
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
        }

        let mut a = net_impl::TcpListenerAccept::new(self)?;
        yield_with_io(&a, a.is_coroutine);
        a.done()
    }

    pub fn incoming(&self) -> Incoming {
        Incoming { listener: self }
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.sys.local_addr()
    }

    #[cfg(not(windows))]
    pub fn try_clone(&self) -> io::Result<TcpListener> {
        self.sys.try_clone().and_then(TcpListener::new)
    }

    // windows doesn't support add dup handler to IOCP
    #[cfg(windows)]
    pub fn try_clone(&self) -> io::Result<TcpListener> {
        let s = self.sys.try_clone()?;
        s.set_nonblocking(true)?;
        io_impl::add_socket(&s).ok();
        Ok(TcpListener {
            _io: io_impl::IoData::new(0),
            sys: s,
        })
    }

    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        self.sys.take_error()
    }

    // TODO: add all std functions
}

#[cfg(unix)]
impl io_impl::AsIoData for TcpListener {
    fn as_io_data(&self) -> &io_impl::IoData {
        &self._io
    }
}

// ===== Incoming =====
//
//

pub struct Incoming<'a> {
    listener: &'a TcpListener,
}

impl<'a> Iterator for Incoming<'a> {
    type Item = io::Result<TcpStream>;
    fn next(&mut self) -> Option<io::Result<TcpStream>> {
        Some(self.listener.accept().map(|p| p.0))
    }
}

// ===== UNIX ext =====
//
//

#[cfg(unix)]
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd, RawFd};

#[cfg(unix)]
impl IntoRawFd for TcpStream {
    fn into_raw_fd(self) -> RawFd {
        self.sys.into_raw_fd()
        // drop self will deregister from the selector
    }
}

#[cfg(unix)]
impl AsRawFd for TcpStream {
    fn as_raw_fd(&self) -> RawFd {
        self.sys.as_raw_fd()
    }
}

#[cfg(unix)]
impl FromRawFd for TcpStream {
    unsafe fn from_raw_fd(fd: RawFd) -> TcpStream {
        TcpStream::new(FromRawFd::from_raw_fd(fd))
            .unwrap_or_else(|e| panic!("from_raw_socket for TcpStream, err = {e:?}"))
    }
}

#[cfg(unix)]
impl IntoRawFd for TcpListener {
    fn into_raw_fd(self) -> RawFd {
        self.sys.into_raw_fd()
        // drop self will deregister from the selector
    }
}

#[cfg(unix)]
impl AsRawFd for TcpListener {
    fn as_raw_fd(&self) -> RawFd {
        self.sys.as_raw_fd()
    }
}

#[cfg(unix)]
impl FromRawFd for TcpListener {
    unsafe fn from_raw_fd(fd: RawFd) -> TcpListener {
        let s: net::TcpListener = FromRawFd::from_raw_fd(fd);
        TcpListener::new(s)
            .unwrap_or_else(|e| panic!("from_raw_socket for TcpListener, err = {e:?}"))
    }
}

impl SplitIo for TcpStream {
    fn split(self) -> io::Result<(SplitReader<Self>, SplitWriter<Self>)> {
        let writer = self.try_clone()?;
        #[cfg(unix)]
        mod_socket(writer.as_io_data(), false)?;
        #[cfg(unix)]
        mod_socket(self.as_io_data(), true)?;
        Ok((SplitReader::new(self), SplitWriter::new(writer)))
    }
}

// ===== Windows ext =====
//
//

#[cfg(windows)]
use std::os::windows::io::{AsRawSocket, FromRawSocket, IntoRawSocket, RawSocket};

#[cfg(windows)]
impl IntoRawSocket for TcpStream {
    fn into_raw_socket(self) -> RawSocket {
        self.sys.into_raw_socket()
    }
}

#[cfg(windows)]
impl AsRawSocket for TcpStream {
    fn as_raw_socket(&self) -> RawSocket {
        self.sys.as_raw_socket()
    }
}

#[cfg(windows)]
impl FromRawSocket for TcpStream {
    unsafe fn from_raw_socket(s: RawSocket) -> TcpStream {
        // TODO: set the time out info here
        // need to set the read/write timeout from sys and sync each other
        TcpStream::new(FromRawSocket::from_raw_socket(s))
            .unwrap_or_else(|e| panic!("from_raw_socket for TcpStream, err = {e:?}"))
    }
}

#[cfg(windows)]
impl IntoRawSocket for TcpListener {
    fn into_raw_socket(self) -> RawSocket {
        self.sys.into_raw_socket()
    }
}

#[cfg(windows)]
impl AsRawSocket for TcpListener {
    fn as_raw_socket(&self) -> RawSocket {
        self.sys.as_raw_socket()
    }
}

#[cfg(windows)]
impl FromRawSocket for TcpListener {
    unsafe fn from_raw_socket(s: RawSocket) -> TcpListener {
        let s: net::TcpListener = FromRawSocket::from_raw_socket(s);
        TcpListener::new(s)
            .unwrap_or_else(|e| panic!("from_raw_socket for TcpListener, err = {e:?}"))
    }
}
