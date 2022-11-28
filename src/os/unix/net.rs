//! Unix-specific networking functionality

use std::fmt;
use std::io;
use std::net::Shutdown;
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd, RawFd};
use std::os::unix::net::{self, SocketAddr};
use std::path::Path;
use std::time::Duration;

use crate::coroutine_impl::is_coroutine;
use crate::io::sys::mod_socket;
use crate::io::sys::net as net_impl;
use crate::io::sys::split_io::{SplitIo, SplitReader, SplitWriter};
use crate::io::CoIo;
use crate::io::{self as io_impl, AsIoData};
use crate::yield_now::yield_with_io;

/// A Unix stream socket.
///
/// # Examples
///
/// ```no_run
/// use may::os::unix::net::UnixStream;
/// use std::io::prelude::*;
///
/// let mut stream = UnixStream::connect("/path/to/my/socket").unwrap();
/// stream.write_all(b"hello world").unwrap();
/// let mut response = String::new();
/// stream.read_to_string(&mut response).unwrap();
/// println!("{}", response);
/// ```
pub struct UnixStream(CoIo<net::UnixStream>);

impl fmt::Debug for UnixStream {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        let mut builder = fmt.debug_struct("UnixStream");
        builder.field("fd", &self.as_raw_fd());
        if let Ok(addr) = self.local_addr() {
            builder.field("local", &addr);
        }
        if let Ok(addr) = self.peer_addr() {
            builder.field("peer", &addr);
        }
        builder.finish()
    }
}

impl UnixStream {
    /// create from CoIo
    pub(crate) fn from_coio(io: CoIo<net::UnixStream>) -> Self {
        UnixStream(io)
    }

    /// Connects to the socket named by `path`.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixStream;
    ///
    /// let socket = match UnixStream::connect("/tmp/sock") {
    ///     Ok(sock) => sock,
    ///     Err(e) => {
    ///         println!("Couldn't connect: {:?}", e);
    ///         return
    ///     }
    /// };
    /// ```
    pub fn connect<P: AsRef<Path>>(path: P) -> io::Result<UnixStream> {
        if !is_coroutine() {
            let stream = net::UnixStream::connect(path)?;
            return Ok(UnixStream(CoIo::new(stream)?));
        }

        let mut c = net_impl::UnixStreamConnect::new(path)?;

        if c.check_connected()? {
            return c.done();
        }

        yield_with_io(&c, c.is_coroutine);
        c.done()
    }

    /// Creates an unnamed pair of connected sockets.
    ///
    /// Returns two `UnixStream`s which are connected to each other.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixStream;
    ///
    /// let (sock1, sock2) = match UnixStream::pair() {
    ///     Ok((sock1, sock2)) => (sock1, sock2),
    ///     Err(e) => {
    ///         println!("Couldn't create a pair of sockets: {:?}", e);
    ///         return
    ///     }
    /// };
    /// ```
    pub fn pair() -> io::Result<(UnixStream, UnixStream)> {
        let (i1, i2) = net::UnixStream::pair()?;
        let i1 = UnixStream(CoIo::new(i1)?);
        let i2 = UnixStream(CoIo::new(i2)?);
        Ok((i1, i2))
    }

    /// Creates a new independently owned handle to the underlying socket.
    ///
    /// The returned `UnixStream` is a reference to the same stream that this
    /// object references. Both handles will read and write the same stream of
    /// data, and options set on one stream will be propagated to the other
    /// stream.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixStream;
    ///
    /// let socket = UnixStream::connect("/tmp/sock").unwrap();
    /// let sock_copy = socket.try_clone().expect("Couldn't clone socket");
    /// ```
    pub fn try_clone(&self) -> io::Result<UnixStream> {
        let stream = self.0.inner().try_clone()?;
        Ok(UnixStream(CoIo::new(stream)?))
    }

    /// Returns the socket address of the local half of this connection.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixStream;
    ///
    /// let socket = UnixStream::connect("/tmp/sock").unwrap();
    /// let addr = socket.local_addr().expect("Couldn't get local address");
    /// ```
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.0.inner().local_addr()
    }

    /// Returns the socket address of the remote half of this connection.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixStream;
    ///
    /// let socket = UnixStream::connect("/tmp/sock").unwrap();
    /// let addr = socket.peer_addr().expect("Couldn't get peer address");
    /// ```
    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.0.inner().peer_addr()
    }

    /// Sets the read timeout for the socket.
    ///
    /// If the provided value is `None`, then `read` calls will block
    /// indefinitely. It is an error to pass the zero `Duration` to this
    /// method.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixStream;
    /// use std::time::Duration;
    ///
    /// let socket = UnixStream::connect("/tmp/sock").unwrap();
    /// socket.set_read_timeout(Some(Duration::new(1, 0))).expect("Couldn't set read timeout");
    /// ```
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        // self.0.inner().set_read_timeout(timeout)?;
        self.0.set_read_timeout(timeout)
    }

    /// Sets the write timeout for the socket.
    ///
    /// If the provided value is `None`, then `write` calls will block
    /// indefinitely. It is an error to pass the zero `Duration` to this
    /// method.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixStream;
    /// use std::time::Duration;
    ///
    /// let socket = UnixStream::connect("/tmp/sock").unwrap();
    /// socket.set_write_timeout(Some(Duration::new(1, 0))).expect("Couldn't set write timeout");
    /// ```
    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.inner().set_write_timeout(timeout)?;
        self.0.set_write_timeout(timeout)
    }

    /// Returns the read timeout of this socket.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixStream;
    /// use std::time::Duration;
    ///
    /// let socket = UnixStream::connect("/tmp/sock").unwrap();
    /// socket.set_read_timeout(Some(Duration::new(1, 0))).expect("Couldn't set read timeout");
    /// assert_eq!(socket.read_timeout().unwrap(), Some(Duration::new(1, 0)));
    /// ```
    pub fn read_timeout(&self) -> io::Result<Option<Duration>> {
        self.0.read_timeout()
    }

    /// Returns the write timeout of this socket.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixStream;
    /// use std::time::Duration;
    ///
    /// let socket = UnixStream::connect("/tmp/sock").unwrap();
    /// socket.set_write_timeout(Some(Duration::new(1, 0))).expect("Couldn't set write timeout");
    /// assert_eq!(socket.write_timeout().unwrap(), Some(Duration::new(1, 0)));
    /// ```
    pub fn write_timeout(&self) -> io::Result<Option<Duration>> {
        self.0.write_timeout()
    }

    /// Moves the socket into or out of nonblocking mode.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixStream;
    ///
    /// let socket = UnixStream::connect("/tmp/sock").unwrap();
    /// socket.set_nonblocking(true).expect("Couldn't set nonblocking");
    /// ```
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.0.set_nonblocking(nonblocking)
    }

    /// Returns the value of the `SO_ERROR` option.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixStream;
    ///
    /// let socket = UnixStream::connect("/tmp/sock").unwrap();
    /// if let Ok(Some(err)) = socket.take_error() {
    ///     println!("Got error: {:?}", err);
    /// }
    /// ```
    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        self.0.inner().take_error()
    }

    /// Shuts down the read, write, or both halves of this connection.
    ///
    /// This function will cause all pending and future I/O calls on the
    /// specified portions to immediately return with an appropriate value
    /// (see the documentation of libstd `Shutdown`).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixStream;
    /// use std::net::Shutdown;
    ///
    /// let socket = UnixStream::connect("/tmp/sock").unwrap();
    /// socket.shutdown(Shutdown::Both).expect("shutdown function failed");
    /// ```
    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        self.0.inner().shutdown(how)
    }
}

impl io::Read for UnixStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

// impl<'a> io::Read for &'a UnixStream {
//     fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
//         (&self.0).read(buf)
//     }
// }

impl io::Write for UnixStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

// impl<'a> io::Write for &'a UnixStream {
//     fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
//         (&self.0).write(buf)
//     }

//     fn flush(&mut self) -> io::Result<()> {
//         (&self.0).flush()
//     }
// }

impl AsRawFd for UnixStream {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

impl FromRawFd for UnixStream {
    unsafe fn from_raw_fd(fd: RawFd) -> UnixStream {
        let stream = FromRawFd::from_raw_fd(fd);
        UnixStream(CoIo::new(stream).expect("can't convert to UnixStream"))
    }
}

impl IntoRawFd for UnixStream {
    fn into_raw_fd(self) -> RawFd {
        self.0.into_raw_fd()
    }
}

impl io_impl::AsIoData for UnixStream {
    fn as_io_data(&self) -> &io_impl::IoData {
        self.0.as_io_data()
    }
}

impl SplitIo for UnixStream {
    fn split(self) -> io::Result<(SplitReader<Self>, SplitWriter<Self>)> {
        let writer = self.try_clone()?;
        mod_socket(writer.as_io_data(), false)?;
        mod_socket(self.as_io_data(), true)?;
        Ok((SplitReader::new(self), SplitWriter::new(writer)))
    }
}

/// A structure representing a Unix domain socket server.
///
/// # Examples
///
/// ```no_run
/// use std::thread;
/// use may::os::unix::net::{UnixStream, UnixListener};
///
/// fn handle_client(stream: UnixStream) {
///     // ...
/// }
///
/// let listener = UnixListener::bind("/path/to/the/socket").unwrap();
///
/// // accept connections and process them, spawning a new thread for each one
/// for stream in listener.incoming() {
///     match stream {
///         Ok(stream) => {
///             /* connection succeeded */
///             thread::spawn(|| handle_client(stream));
///         }
///         Err(err) => {
///             /* connection failed */
///             break;
///         }
///     }
/// }
/// ```
pub struct UnixListener(pub(crate) CoIo<net::UnixListener>);

impl fmt::Debug for UnixListener {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        let mut builder = fmt.debug_struct("UnixListener");
        builder.field("fd", &self.as_raw_fd());
        if let Ok(addr) = self.local_addr() {
            builder.field("local", &addr);
        }
        builder.finish()
    }
}

impl UnixListener {
    /// Creates a new `UnixListener` bound to the specified socket.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixListener;
    ///
    /// let listener = match UnixListener::bind("/path/to/the/socket") {
    ///     Ok(sock) => sock,
    ///     Err(e) => {
    ///         println!("Couldn't connect: {:?}", e);
    ///         return
    ///     }
    /// };
    /// ```
    pub fn bind<P: AsRef<Path>>(path: P) -> io::Result<UnixListener> {
        let listener = net::UnixListener::bind(path)?;
        Ok(UnixListener(CoIo::new(listener)?))
    }

    /// Accepts a new incoming connection to this listener.
    ///
    /// This function will block the calling thread until a new Unix connection
    /// is established. When established, the corresponding `UnixStream` and
    /// the remote peer's address will be returned.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixListener;
    ///
    /// let listener = UnixListener::bind("/path/to/the/socket").unwrap();
    ///
    /// match listener.accept() {
    ///     Ok((socket, addr)) => println!("Got a client: {:?}", addr),
    ///     Err(e) => println!("accept function failed: {:?}", e),
    /// }
    /// ```
    pub fn accept(&self) -> io::Result<(UnixStream, SocketAddr)> {
        if self.0.check_nonblocking() {
            let (s, a) = self.0.inner().accept()?;
            return Ok((UnixStream(CoIo::new(s)?), a));
        }

        self.0.io_reset();
        match self.0.inner().accept() {
            Ok((s, a)) => return Ok((UnixStream(CoIo::new(s)?), a)),
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

        let mut a = net_impl::UnixListenerAccept::new(self)?;
        yield_with_io(&a, a.is_coroutine);
        a.done()
    }

    /// Creates a new independently owned handle to the underlying socket.
    ///
    /// The returned `UnixListener` is a reference to the same socket that this
    /// object references. Both handles can be used to accept incoming
    /// connections and options set on one listener will affect the other.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixListener;
    ///
    /// let listener = UnixListener::bind("/path/to/the/socket").unwrap();
    ///
    /// let listener_copy = listener.try_clone().expect("try_clone failed");
    /// ```
    pub fn try_clone(&self) -> io::Result<UnixListener> {
        let listener = self.0.inner().try_clone()?;
        Ok(UnixListener(CoIo::new(listener)?))
    }

    /// Returns the local socket address of this listener.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixListener;
    ///
    /// let listener = UnixListener::bind("/path/to/the/socket").unwrap();
    ///
    /// let addr = listener.local_addr().expect("Couldn't get local address");
    /// ```
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.0.inner().local_addr()
    }

    /// Moves the socket into or out of nonblocking mode.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixListener;
    ///
    /// let listener = UnixListener::bind("/path/to/the/socket").unwrap();
    ///
    /// listener.set_nonblocking(true).expect("Couldn't set non blocking");
    /// ```
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.0.set_nonblocking(nonblocking)
    }

    /// Returns the value of the `SO_ERROR` option.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixListener;
    ///
    /// let listener = UnixListener::bind("/tmp/sock").unwrap();
    ///
    /// if let Ok(Some(err)) = listener.take_error() {
    ///     println!("Got error: {:?}", err);
    /// }
    /// ```
    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        self.0.inner().take_error()
    }

    /// Returns an iterator over incoming connections.
    ///
    /// The iterator will never return `None` and will also not yield the
    /// peer's `SocketAddr` structure.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use std::thread;
    /// use may::os::unix::net::{UnixStream, UnixListener};
    ///
    /// fn handle_client(stream: UnixStream) {
    ///     // ...
    /// }
    ///
    /// let listener = UnixListener::bind("/path/to/the/socket").unwrap();
    ///
    /// for stream in listener.incoming() {
    ///     match stream {
    ///         Ok(stream) => {
    ///             thread::spawn(|| handle_client(stream));
    ///         }
    ///         Err(err) => {
    ///             break;
    ///         }
    ///     }
    /// }
    /// ```
    pub fn incoming(&self) -> Incoming {
        Incoming { listener: self }
    }
}

impl AsRawFd for UnixListener {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

impl FromRawFd for UnixListener {
    unsafe fn from_raw_fd(fd: RawFd) -> UnixListener {
        let listener = FromRawFd::from_raw_fd(fd);
        UnixListener(CoIo::new(listener).expect("can't convert to UnixListener"))
    }
}

impl IntoRawFd for UnixListener {
    fn into_raw_fd(self) -> RawFd {
        self.0.into_raw_fd()
    }
}

impl<'a> IntoIterator for &'a UnixListener {
    type Item = io::Result<UnixStream>;
    type IntoIter = Incoming<'a>;

    fn into_iter(self) -> Incoming<'a> {
        self.incoming()
    }
}

/// An iterator over incoming connections to a `UnixListener`.
///
/// It will never return `None`.
///
/// # Examples
///
/// ```no_run
/// use std::thread;
/// use may::os::unix::net::{UnixStream, UnixListener};
///
/// fn handle_client(stream: UnixStream) {
///     // ...
/// }
///
/// let listener = UnixListener::bind("/path/to/the/socket").unwrap();
///
/// for stream in listener.incoming() {
///     match stream {
///         Ok(stream) => {
///             thread::spawn(|| handle_client(stream));
///         }
///         Err(err) => {
///             break;
///         }
///     }
/// }
/// ```
#[derive(Debug)]
pub struct Incoming<'a> {
    listener: &'a UnixListener,
}

impl<'a> Iterator for Incoming<'a> {
    type Item = io::Result<UnixStream>;

    fn next(&mut self) -> Option<io::Result<UnixStream>> {
        Some(self.listener.accept().map(|s| s.0))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (usize::max_value(), None)
    }
}

/// A Unix datagram socket.
///
/// # Examples
///
/// ```no_run
/// use may::os::unix::net::UnixDatagram;
///
/// let socket = UnixDatagram::bind("/path/to/my/socket").unwrap();
/// socket.send_to(b"hello world", "/path/to/other/socket").unwrap();
/// let mut buf = [0; 100];
/// let (count, address) = socket.recv_from(&mut buf).unwrap();
/// println!("socket {:?} sent {:?}", address, &buf[..count]);
/// ```
pub struct UnixDatagram(pub(crate) CoIo<net::UnixDatagram>);

impl fmt::Debug for UnixDatagram {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        let mut builder = fmt.debug_struct("UnixDatagram");
        builder.field("fd", &self.as_raw_fd());
        if let Ok(addr) = self.local_addr() {
            builder.field("local", &addr);
        }
        if let Ok(addr) = self.peer_addr() {
            builder.field("peer", &addr);
        }
        builder.finish()
    }
}

impl UnixDatagram {
    /// Creates a Unix datagram socket bound to the given path.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixDatagram;
    ///
    /// let sock = match UnixDatagram::bind("/path/to/the/socket") {
    ///     Ok(sock) => sock,
    ///     Err(e) => {
    ///         println!("Couldn't bind: {:?}", e);
    ///         return
    ///     }
    /// };
    /// ```
    pub fn bind<P: AsRef<Path>>(path: P) -> io::Result<UnixDatagram> {
        let datagram = net::UnixDatagram::bind(path)?;
        Ok(UnixDatagram(CoIo::new(datagram)?))
    }

    /// Creates a Unix Datagram socket which is not bound to any address.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixDatagram;
    ///
    /// let sock = match UnixDatagram::unbound() {
    ///     Ok(sock) => sock,
    ///     Err(e) => {
    ///         println!("Couldn't unbound: {:?}", e);
    ///         return
    ///     }
    /// };
    /// ```
    pub fn unbound() -> io::Result<UnixDatagram> {
        let datagram = net::UnixDatagram::unbound()?;
        Ok(UnixDatagram(CoIo::new(datagram)?))
    }

    /// Create an unnamed pair of connected sockets.
    ///
    /// Returns two `UnixDatagram`s which are connected to each other.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixDatagram;
    ///
    /// let (sock1, sock2) = match UnixDatagram::pair() {
    ///     Ok((sock1, sock2)) => (sock1, sock2),
    ///     Err(e) => {
    ///         println!("Couldn't unbound: {:?}", e);
    ///         return
    ///     }
    /// };
    /// ```
    pub fn pair() -> io::Result<(UnixDatagram, UnixDatagram)> {
        let (i1, i2) = net::UnixDatagram::pair()?;
        let i1 = UnixDatagram(CoIo::new(i1)?);
        let i2 = UnixDatagram(CoIo::new(i2)?);
        Ok((i1, i2))
    }

    /// Connects the socket to the specified address.
    ///
    /// The [`send`] method may be used to send data to the specified address.
    /// [`recv`] and [`recv_from`] will only receive data from that address.
    ///
    /// [`send`]: #method.send
    /// [`recv`]: #method.recv
    /// [`recv_from`]: #method.recv_from
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixDatagram;
    ///
    /// let sock = UnixDatagram::unbound().unwrap();
    /// match sock.connect("/path/to/the/socket") {
    ///     Ok(sock) => sock,
    ///     Err(e) => {
    ///         println!("Couldn't connect: {:?}", e);
    ///         return
    ///     }
    /// };
    /// ```
    pub fn connect<P: AsRef<Path>>(&self, path: P) -> io::Result<()> {
        // for UnixDatagram connect it's a nonblocking operation
        // so we just use the system call
        self.0.inner().connect(path)
    }

    /// Creates a new independently owned handle to the underlying socket.
    ///
    /// The returned `UnixDatagram` is a reference to the same socket that this
    /// object references. Both handles can be used to accept incoming
    /// connections and options set on one side will affect the other.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixDatagram;
    ///
    /// let sock = UnixDatagram::bind("/path/to/the/socket").unwrap();
    ///
    /// let sock_copy = sock.try_clone().expect("try_clone failed");
    /// ```
    pub fn try_clone(&self) -> io::Result<UnixDatagram> {
        let datagram = self.0.inner().try_clone()?;
        Ok(UnixDatagram(CoIo::new(datagram)?))
    }

    /// Returns the address of this socket.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixDatagram;
    ///
    /// let sock = UnixDatagram::bind("/path/to/the/socket").unwrap();
    ///
    /// let addr = sock.local_addr().expect("Couldn't get local address");
    /// ```
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.0.inner().local_addr()
    }

    /// Returns the address of this socket's peer.
    ///
    /// The [`connect`] method will connect the socket to a peer.
    ///
    /// [`connect`]: #method.connect
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixDatagram;
    ///
    /// let sock = UnixDatagram::unbound().unwrap();
    /// sock.connect("/path/to/the/socket").unwrap();
    ///
    /// let addr = sock.peer_addr().expect("Couldn't get peer address");
    /// ```
    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.0.inner().peer_addr()
    }

    /// Receives data from the socket.
    ///
    /// On success, returns the number of bytes read and the address from
    /// whence the data came.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixDatagram;
    ///
    /// let sock = UnixDatagram::unbound().unwrap();
    /// let mut buf = vec![0; 10];
    /// match sock.recv_from(buf.as_mut_slice()) {
    ///     Ok((size, sender)) => println!("received {} bytes from {:?}", size, sender),
    ///     Err(e) => println!("recv_from function failed: {:?}", e),
    /// }
    /// ```
    pub fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        if self.0.check_nonblocking() {
            return self.0.inner().recv_from(buf);
        }

        self.0.io_reset();
        // this is an earlier return try for nonblocking read
        match self.0.inner().recv_from(buf) {
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

        let mut reader = net_impl::UnixRecvFrom::new(self, buf);
        yield_with_io(&reader, reader.is_coroutine);
        reader.done()
    }

    /// Receives data from the socket.
    ///
    /// On success, returns the number of bytes read.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixDatagram;
    ///
    /// let sock = UnixDatagram::bind("/path/to/the/socket").unwrap();
    /// let mut buf = vec![0; 10];
    /// sock.recv(buf.as_mut_slice()).expect("recv function failed");
    /// ```
    pub fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        if self.0.check_nonblocking() {
            return self.0.inner().recv(buf);
        }

        self.0.io_reset();
        // this is an earlier return try for nonblocking read
        match self.0.inner().recv(buf) {
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

        let mut reader = net_impl::SocketRead::new(&self.0, buf, self.read_timeout().unwrap());
        yield_with_io(&reader, reader.is_coroutine);
        reader.done()
    }

    /// Sends data on the socket to the specified address.
    ///
    /// On success, returns the number of bytes written.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixDatagram;
    ///
    /// let sock = UnixDatagram::unbound().unwrap();
    /// sock.send_to(b"omelette au fromage", "/some/sock").expect("send_to function failed");
    /// ```
    pub fn send_to<P: AsRef<Path>>(&self, buf: &[u8], path: P) -> io::Result<usize> {
        if self.0.check_nonblocking() {
            return self.0.inner().send_to(buf, path);
        }

        self.0.io_reset();
        // this is an earlier return try for nonblocking read
        match self.0.inner().send_to(buf, path.as_ref()) {
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

        let mut writer = net_impl::UnixSendTo::new(self, buf, path.as_ref())?;
        yield_with_io(&writer, writer.is_coroutine);
        writer.done()
    }

    /// Sends data on the socket to the socket's peer.
    ///
    /// The peer address may be set by the `connect` method, and this method
    /// will return an error if the socket has not already been connected.
    ///
    /// On success, returns the number of bytes written.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixDatagram;
    ///
    /// let sock = UnixDatagram::unbound().unwrap();
    /// sock.connect("/some/sock").expect("Couldn't connect");
    /// sock.send(b"omelette au fromage").expect("send_to function failed");
    /// ```
    pub fn send(&self, buf: &[u8]) -> io::Result<usize> {
        if self.0.check_nonblocking() {
            return self.0.inner().send(buf);
        }

        self.0.io_reset();
        // this is an earlier return try for nonblocking write
        match self.0.inner().send(buf) {
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

        let mut writer = net_impl::SocketWrite::new(&self.0, buf, self.0.write_timeout().unwrap());
        yield_with_io(&writer, writer.is_coroutine);
        writer.done()
    }

    /// Sets the read timeout for the socket.
    ///
    /// If the provided value is `None`, then [`recv`] and [`recv_from`] calls will
    /// block indefinitely. It is an error to pass the zero `Duration` to this
    /// method.
    ///
    /// [`recv`]: #method.recv
    /// [`recv_from`]: #method.recv_from
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixDatagram;
    /// use std::time::Duration;
    ///
    /// let sock = UnixDatagram::unbound().unwrap();
    /// sock.set_read_timeout(Some(Duration::new(1, 0))).expect("set_read_timeout function failed");
    /// ```
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.inner().set_read_timeout(timeout)?;
        self.0.set_read_timeout(timeout)
    }

    /// Sets the write timeout for the socket.
    ///
    /// If the provided value is `None`, then [`send`] and [`send_to`] calls will
    /// block indefinitely. It is an error to pass the zero `Duration` to this
    /// method.
    ///
    /// [`send`]: #method.send
    /// [`send_to`]: #method.send_to
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixDatagram;
    /// use std::time::Duration;
    ///
    /// let sock = UnixDatagram::unbound().unwrap();
    /// sock.set_write_timeout(Some(Duration::new(1, 0)))
    ///     .expect("set_write_timeout function failed");
    /// ```
    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.inner().set_write_timeout(timeout)?;
        self.0.set_write_timeout(timeout)
    }

    /// Returns the read timeout of this socket.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixDatagram;
    /// use std::time::Duration;
    ///
    /// let sock = UnixDatagram::unbound().unwrap();
    /// sock.set_read_timeout(Some(Duration::new(1, 0))).expect("set_read_timeout function failed");
    /// assert_eq!(sock.read_timeout().unwrap(), Some(Duration::new(1, 0)));
    /// ```
    pub fn read_timeout(&self) -> io::Result<Option<Duration>> {
        self.0.read_timeout()
    }

    /// Returns the write timeout of this socket.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixDatagram;
    /// use std::time::Duration;
    ///
    /// let sock = UnixDatagram::unbound().unwrap();
    /// sock.set_write_timeout(Some(Duration::new(1, 0)))
    ///     .expect("set_write_timeout function failed");
    /// assert_eq!(sock.write_timeout().unwrap(), Some(Duration::new(1, 0)));
    /// ```
    pub fn write_timeout(&self) -> io::Result<Option<Duration>> {
        self.0.write_timeout()
    }

    /// Moves the socket into or out of nonblocking mode.
    ///
    /// # Examples
    ///
    /// ```
    /// use may::os::unix::net::UnixDatagram;
    ///
    /// let sock = UnixDatagram::unbound().unwrap();
    /// sock.set_nonblocking(true).expect("set_nonblocking function failed");
    /// ```
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.0.set_nonblocking(nonblocking)
    }

    /// Returns the value of the `SO_ERROR` option.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixDatagram;
    ///
    /// let sock = UnixDatagram::unbound().unwrap();
    /// if let Ok(Some(err)) = sock.take_error() {
    ///     println!("Got error: {:?}", err);
    /// }
    /// ```
    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        self.0.inner().take_error()
    }

    /// Shut down the read, write, or both halves of this connection.
    ///
    /// This function will cause all pending and future I/O calls on the
    /// specified portions to immediately return with an appropriate value
    /// (see the documentation of libstd `Shutdown`).
    ///
    /// ```no_run
    /// use may::os::unix::net::UnixDatagram;
    /// use std::net::Shutdown;
    ///
    /// let sock = UnixDatagram::unbound().unwrap();
    /// sock.shutdown(Shutdown::Both).expect("shutdown function failed");
    /// ```
    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        self.0.inner().shutdown(how)
    }
}

impl AsRawFd for UnixDatagram {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

impl FromRawFd for UnixDatagram {
    unsafe fn from_raw_fd(fd: RawFd) -> UnixDatagram {
        let datagram = FromRawFd::from_raw_fd(fd);
        UnixDatagram(CoIo::new(datagram).expect("can't convert to UnixDatagram"))
    }
}

impl IntoRawFd for UnixDatagram {
    fn into_raw_fd(self) -> RawFd {
        self.0.into_raw_fd()
    }
}

impl io_impl::AsIoData for UnixDatagram {
    fn as_io_data(&self) -> &io_impl::IoData {
        self.0.as_io_data()
    }
}

#[cfg(all(test, not(target_os = "emscripten")))]
mod test {
    use std::io;
    use std::io::prelude::*;
    use std::time::Duration;
    use tempdir::TempDir;

    use super::*;

    fn tmpdir() -> TempDir {
        TempDir::new("test").expect("failed to create TempDir")
    }

    macro_rules! or_panic {
        ($e:expr) => {
            match $e {
                Ok(e) => e,
                Err(e) => panic!("{}", e),
            }
        };
    }

    #[test]
    fn basic() {
        let dir = tmpdir();
        let socket_path = dir.path().join("sock");
        let msg1 = b"hello";
        let msg2 = b"world!";

        let listener = or_panic!(UnixListener::bind(&socket_path));
        let thread = go!(move || {
            let mut stream = or_panic!(listener.accept()).0;
            let mut buf = [0; 5];
            or_panic!(stream.read_exact(&mut buf));
            assert_eq!(&msg1[..], &buf[..]);
            or_panic!(stream.write_all(msg2));
        });

        let mut stream = or_panic!(UnixStream::connect(&socket_path));
        assert_eq!(
            Some(&*socket_path),
            stream.peer_addr().unwrap().as_pathname()
        );
        or_panic!(stream.write_all(msg1));
        let mut buf = vec![];
        or_panic!(stream.read_to_end(&mut buf));
        assert_eq!(&msg2[..], &buf[..]);
        drop(stream);

        thread.join().unwrap();
    }

    #[test]
    fn pair() {
        let msg1 = b"hello";
        let msg2 = b"world!";

        let (mut s1, mut s2) = or_panic!(UnixStream::pair());
        let thread = go!(move || {
            // s1 must be moved in or the test will hang!
            let mut buf = [0; 5];
            or_panic!(s1.read_exact(&mut buf));
            assert_eq!(&msg1[..], &buf[..]);
            or_panic!(s1.write_all(msg2));
        });

        or_panic!(s2.write_all(msg1));
        let mut buf = vec![];
        or_panic!(s2.read_to_end(&mut buf));
        assert_eq!(&msg2[..], &buf[..]);
        drop(s2);

        thread.join().unwrap();
    }

    #[test]
    fn try_clone() {
        let dir = tmpdir();
        let socket_path = dir.path().join("sock");
        let msg1 = b"hello";
        let msg2 = b"world";

        let listener = or_panic!(UnixListener::bind(&socket_path));
        let thread = go!(move || {
            let mut stream = or_panic!(listener.accept()).0;
            or_panic!(stream.write_all(msg1));
            or_panic!(stream.write_all(msg2));
        });

        let mut stream = or_panic!(UnixStream::connect(&socket_path));
        let mut stream2 = or_panic!(stream.try_clone());

        let mut buf = [0; 5];
        or_panic!(stream.read_exact(&mut buf));
        assert_eq!(&msg1[..], &buf[..]);
        or_panic!(stream2.read_exact(&mut buf));
        assert_eq!(&msg2[..], &buf[..]);

        thread.join().unwrap();
    }

    #[test]
    fn iter() {
        let dir = tmpdir();
        let socket_path = dir.path().join("sock");

        let listener = or_panic!(UnixListener::bind(&socket_path));
        let thread = go!(move || for stream in listener.incoming().take(2) {
            let mut stream = or_panic!(stream);
            let mut buf = [0];
            or_panic!(stream.read_exact(&mut buf));
        });

        for _ in 0..2 {
            let mut stream = or_panic!(UnixStream::connect(&socket_path));
            or_panic!(stream.write_all(&[0]));
        }

        thread.join().unwrap();
    }

    #[test]
    fn long_path() {
        let dir = tmpdir();
        let socket_path = dir.path().join(
            "asdfasdfasdfasdfasdfasdfasdfasdfasdfasdfasdfasdfasdfasdfasdfa\
             sasdfasdfasdasdfasdfasdfadfasdfasdfasdfasdfasdf",
        );
        match UnixStream::connect(&socket_path) {
            Err(ref e) if e.kind() == io::ErrorKind::InvalidInput => {}
            Err(e) => panic!("unexpected error {}", e),
            Ok(_) => panic!("unexpected success"),
        }

        match UnixListener::bind(&socket_path) {
            Err(ref e) if e.kind() == io::ErrorKind::InvalidInput => {}
            Err(e) => panic!("unexpected error {}", e),
            Ok(_) => panic!("unexpected success"),
        }

        match UnixDatagram::bind(&socket_path) {
            Err(ref e) if e.kind() == io::ErrorKind::InvalidInput => {}
            Err(e) => panic!("unexpected error {}", e),
            Ok(_) => panic!("unexpected success"),
        }
    }

    #[test]
    fn timeouts() {
        let dir = tmpdir();
        let socket_path = dir.path().join("sock");

        let _listener = or_panic!(UnixListener::bind(&socket_path));

        let stream = or_panic!(UnixStream::connect(&socket_path));
        let dur = Duration::new(15410, 0);

        assert_eq!(None, or_panic!(stream.read_timeout()));

        or_panic!(stream.set_read_timeout(Some(dur)));
        assert_eq!(Some(dur), or_panic!(stream.read_timeout()));

        assert_eq!(None, or_panic!(stream.write_timeout()));

        or_panic!(stream.set_write_timeout(Some(dur)));
        assert_eq!(Some(dur), or_panic!(stream.write_timeout()));

        or_panic!(stream.set_read_timeout(None));
        assert_eq!(None, or_panic!(stream.read_timeout()));

        or_panic!(stream.set_write_timeout(None));
        assert_eq!(None, or_panic!(stream.write_timeout()));
    }

    #[test]
    fn test_read_timeout() {
        let dir = tmpdir();
        let socket_path = dir.path().join("sock");

        let _listener = or_panic!(UnixListener::bind(&socket_path));

        let mut stream = or_panic!(UnixStream::connect(&socket_path));
        or_panic!(stream.set_read_timeout(Some(Duration::from_millis(1000))));

        let mut buf = [0; 10];
        let kind = stream.read(&mut buf).expect_err("expected error").kind();
        assert!(kind == io::ErrorKind::WouldBlock || kind == io::ErrorKind::TimedOut);
    }

    #[test]
    fn test_read_with_timeout() {
        let dir = tmpdir();
        let socket_path = dir.path().join("sock");

        let listener = or_panic!(UnixListener::bind(&socket_path));

        let mut stream = or_panic!(UnixStream::connect(&socket_path));
        or_panic!(stream.set_read_timeout(Some(Duration::from_millis(1000))));

        let mut other_end = or_panic!(listener.accept()).0;
        or_panic!(other_end.write_all(b"hello world"));

        let mut buf = [0; 11];
        or_panic!(stream.read_exact(&mut buf));
        assert_eq!(b"hello world", &buf[..]);

        let kind = stream
            .read_exact(&mut buf)
            .expect_err("expected error")
            .kind();
        assert!(kind == io::ErrorKind::WouldBlock || kind == io::ErrorKind::TimedOut);
    }

    #[test]
    fn test_unix_datagram() {
        let dir = tmpdir();
        let path1 = dir.path().join("sock1");
        let path2 = dir.path().join("sock2");

        let sock1 = or_panic!(UnixDatagram::bind(path1));
        let sock2 = or_panic!(UnixDatagram::bind(&path2));

        let msg = b"hello world";
        or_panic!(sock1.send_to(msg, &path2));
        let mut buf = [0; 11];
        or_panic!(sock2.recv_from(&mut buf));
        assert_eq!(msg, &buf[..]);
    }

    #[test]
    fn test_unnamed_unix_datagram() {
        let dir = tmpdir();
        let path1 = dir.path().join("sock1");

        let sock1 = or_panic!(UnixDatagram::bind(&path1));
        let sock2 = or_panic!(UnixDatagram::unbound());

        let msg = b"hello world";
        or_panic!(sock2.send_to(msg, &path1));
        let mut buf = [0; 11];
        let (usize, addr) = or_panic!(sock1.recv_from(&mut buf));
        assert_eq!(usize, 11);
        assert!(addr.is_unnamed());
        assert_eq!(msg, &buf[..]);
    }

    #[test]
    fn test_connect_unix_datagram() {
        let dir = tmpdir();
        let path1 = dir.path().join("sock1");
        let path2 = dir.path().join("sock2");

        let bsock1 = or_panic!(UnixDatagram::bind(&path1));
        let bsock2 = or_panic!(UnixDatagram::bind(&path2));
        let sock = or_panic!(UnixDatagram::unbound());
        or_panic!(sock.connect(&path1));

        // Check send()
        let msg = b"hello there";
        or_panic!(sock.send(msg));
        let mut buf = [0; 11];
        let (usize, addr) = or_panic!(bsock1.recv_from(&mut buf));
        assert_eq!(usize, 11);
        assert!(addr.is_unnamed());
        assert_eq!(msg, &buf[..]);

        // Changing default socket works too
        or_panic!(sock.connect(&path2));
        or_panic!(sock.send(msg));
        or_panic!(bsock2.recv_from(&mut buf));
    }

    #[test]
    fn test_unix_datagram_recv() {
        let dir = tmpdir();
        let path1 = dir.path().join("sock1");

        let sock1 = or_panic!(UnixDatagram::bind(&path1));
        let sock2 = or_panic!(UnixDatagram::unbound());
        or_panic!(sock2.connect(&path1));

        let msg = b"hello world";
        or_panic!(sock2.send(msg));
        let mut buf = [0; 11];
        let size = or_panic!(sock1.recv(&mut buf));
        assert_eq!(size, 11);
        assert_eq!(msg, &buf[..]);
    }

    #[test]
    fn datagram_pair() {
        let msg1 = b"hello";
        let msg2 = b"world!";

        let (s1, s2) = or_panic!(UnixDatagram::pair());
        let thread = go!(move || {
            // s1 must be moved in or the test will hang!
            let mut buf = [0; 5];
            or_panic!(s1.recv(&mut buf));
            assert_eq!(&msg1[..], &buf[..]);
            or_panic!(s1.send(msg2));
        });

        or_panic!(s2.send(msg1));
        let mut buf = [0; 6];
        or_panic!(s2.recv(&mut buf));
        assert_eq!(&msg2[..], &buf[..]);
        drop(s2);

        thread.join().unwrap();
    }

    #[test]
    fn abstract_namespace_not_allowed() {
        assert!(UnixStream::connect("\0asdf").is_err());
    }
}
