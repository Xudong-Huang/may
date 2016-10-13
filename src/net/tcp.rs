use std::time::Duration;
use std::io::{self, Read, Write};
use std::net::{self, ToSocketAddrs, SocketAddr, Shutdown};
use io as io_impl;
use io::net as net_impl;
use yield_now::yield_with;
use coroutine::is_coroutine;

extern crate nix;
use self::nix::fcntl::FcntlArg::F_SETFL;
use self::nix::fcntl::{fcntl, O_NONBLOCK};

// ===== TcpStream =====
//
//

#[derive(Debug)]
pub struct TcpStream {
    io: io_impl::IoData,
    sys: net::TcpStream,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
}

impl TcpStream {
    pub fn new(s: net::TcpStream) -> io::Result<TcpStream> {
        // only set non blocking in coroutine context
        // we would first call nonblocking io in the coroutine
        // to avoid unnecessary context switch
        // try!(s.set_nonblocking(true));
        try!(fcntl(s.as_raw_fd(), F_SETFL(O_NONBLOCK)));

        io_impl::add_socket(&s).map(|io| {
            TcpStream {
                io: io,
                sys: s,
                read_timeout: None,
                write_timeout: None,
            }
        })
    }

    pub fn inner(&self) -> &net::TcpStream {
        &self.sys
    }

    pub fn connect<A: ToSocketAddrs>(addr: A) -> io::Result<TcpStream> {
        if !is_coroutine() {
            let s = try!(net::TcpStream::connect(addr));
            let fd = s.as_raw_fd();
            return Ok(TcpStream::from_stream(s, io_impl::IoData::new(fd)));
        }

        let c = try!(net_impl::TcpStreamConnect::new(addr));
        yield_with(&c);
        c.done()
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.sys.peer_addr()
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.sys.local_addr()
    }

    pub fn try_clone(&self) -> io::Result<TcpStream> {
        let s = try!(self.sys.try_clone().and_then(|s| TcpStream::new(s)));
        s.set_read_timeout(self.read_timeout).unwrap();
        s.set_write_timeout(self.write_timeout).unwrap();
        Ok(s)
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

    pub fn set_read_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        let me = unsafe { &mut *(self as *const _ as *mut Self) };
        me.read_timeout = dur;
        Ok(())
    }

    pub fn set_write_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        let me = unsafe { &mut *(self as *const _ as *mut Self) };
        me.write_timeout = dur;
        Ok(())
    }

    pub fn read_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(self.read_timeout)
    }

    pub fn write_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(self.write_timeout)
    }

    // convert std::net::TcpStream to Self without add_socket
    #[cfg(unix)]
    pub fn from_stream(s: net::TcpStream, io: io_impl::IoData) -> Self {
        TcpStream {
            io: io,
            sys: s,
            read_timeout: None,
            write_timeout: None,
        }
    }
}

impl Read for TcpStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !is_coroutine() {
            // this can't be nonblocking!!
            try!(self.sys.set_nonblocking(false));
            let ret = try!(self.sys.read(buf));
            try!(self.sys.set_nonblocking(true));;
            return Ok(ret);
        }

        self.io.reset();
        // this is an earlier return try for nonblocking read
        // it's useful for server but not necessary for client
        match self.sys.read(buf) {
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
            ret @ _ => return ret,
        }

        let reader = net_impl::SocketRead::new(self, buf, self.read_timeout);
        yield_with(&reader);
        reader.done()
    }
}

impl Write for TcpStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !is_coroutine() {
            // in the thread context, just use the block version
            try!(self.sys.set_nonblocking(false));
            let ret = try!(self.sys.write(buf));
            try!(self.sys.set_nonblocking(true));;
            return Ok(ret);
        }

        self.io.reset();
        // this is an earlier return try for nonblocking write
        match self.sys.write(buf) {
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
            ret @ _ => return ret,
        }

        let writer = net_impl::SocketWrite::new(self, buf, self.write_timeout);
        yield_with(&writer);
        writer.done()
    }

    fn flush(&mut self) -> io::Result<()> {
        // TcpStream just return Ok(()), no need to yield
        (&self.sys).flush()
    }
}

impl Drop for TcpStream {
    fn drop(&mut self) {
        io_impl::del_socket(&self.io);
    }
}

impl io_impl::AsEventData for TcpStream {
    fn as_event_data(&self) -> &mut io_impl::EventData {
        self.io.inner()
    }
}


// ===== TcpListener =====
//
//

#[derive(Debug)]
pub struct TcpListener {
    io: io_impl::IoData,
    sys: net::TcpListener,
}

impl TcpListener {
    pub fn new(s: net::TcpListener) -> io::Result<TcpListener> {
        // only set non blocking in coroutine context
        // we would first call nonblocking io in the coroutine
        // to avoid unnecessary context switch
        try!(s.set_nonblocking(true));
        // try!(s.reuse_address(true));
        io_impl::add_socket(&s).map(|io| TcpListener { io: io, sys: s })
    }

    pub fn inner(&self) -> &net::TcpListener {
        &self.sys
    }

    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<TcpListener> {
        let s = try!(net::TcpListener::bind(addr));
        TcpListener::new(s)
    }

    pub fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        if !is_coroutine() {
            let (s, a) = try!(self.sys.accept());
            return TcpStream::new(s).map(|s| (s, a));
        }

        self.io.reset();
        match self.sys.accept() {
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
            ret @ _ => return ret.and_then(|(s, a)| TcpStream::new(s).map(|s| (s, a))),
        }

        let a = try!(net_impl::TcpListenerAccept::new(self));
        yield_with(&a);
        a.done()
    }

    pub fn incoming(&self) -> Incoming {
        Incoming { listener: self }
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.sys.local_addr()
    }

    pub fn try_clone(&self) -> io::Result<TcpListener> {
        self.sys.try_clone().and_then(|s| TcpListener::new(s))
    }

    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        self.sys.take_error()
    }

    // TODO: add all std functions
}

impl Drop for TcpListener {
    fn drop(&mut self) {
        io_impl::del_socket(&self.io);
    }
}

impl io_impl::AsEventData for TcpListener {
    fn as_event_data(&self) -> &mut io_impl::EventData {
        self.io.inner()
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
use std::os::unix::io::{IntoRawFd, AsRawFd, FromRawFd, RawFd};

#[cfg(unix)]
impl IntoRawFd for TcpStream {
    fn into_raw_fd(self) -> RawFd {
        self.sys.as_raw_fd()
        // drop self will dereg from the selector
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
            .unwrap_or_else(|e| panic!("from_raw_socket for TcpStream, err = {:?}", e))
    }
}

#[cfg(unix)]
impl IntoRawFd for TcpListener {
    fn into_raw_fd(self) -> RawFd {
        self.sys.as_raw_fd()
        // drop self will dereg from the selector
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
            .unwrap_or_else(|e| panic!("from_raw_socket for TcpListener, err = {:?}", e))
    }
}

// ===== Windows ext =====
//
//

#[cfg(windows)]
use std::os::windows::io::{IntoRawSocket, AsRawSocket, FromRawSocket, RawSocket};

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
            .unwrap_or_else(|e| panic!("from_raw_socket for TcpStream, err = {:?}", e))
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
            .unwrap_or_else(|e| panic!("from_raw_socket for TcpListener, err = {:?}", e));
        TcpListener { sys: s }
    }
}
