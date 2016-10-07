use io::net as net_impl;
use std::io;
use std::time::Duration;
use std::net::{self, ToSocketAddrs, SocketAddr, Ipv4Addr, Ipv6Addr};
use yield_now::yield_with;
use coroutine::is_coroutine;

#[derive(Debug)]
pub struct UdpSocket {
    sys: net::UdpSocket,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
}

impl UdpSocket {
    pub fn new(s: net::UdpSocket) -> io::Result<UdpSocket> {
        // only set non blocking in coroutine context
        // we would first call nonblocking io in the coroutine
        // to avoid unnecessary context switch
        try!(s.set_nonblocking(true));

        net_impl::add_socket(&s).map(|_| {
            UdpSocket {
                sys: s,
                read_timeout: None,
                write_timeout: None,
            }
        })
    }

    pub fn inner(&self) -> &net::UdpSocket {
        &self.sys
    }

    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<UdpSocket> {
        net::UdpSocket::bind(addr).and_then(|s| UdpSocket::new(s))
    }

    pub fn connect<A: ToSocketAddrs>(&self, addr: A) -> io::Result<()> {
        self.sys.connect(addr)
    }

    /// Returns the socket address that this socket was created from.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.sys.local_addr()
    }

    pub fn try_clone(&self) -> io::Result<UdpSocket> {
        self.sys.try_clone().map(|s| {
            UdpSocket {
                sys: s,
                read_timeout: self.read_timeout,
                write_timeout: self.read_timeout,
            }
        })
    }

    pub fn send_to<A: ToSocketAddrs>(&self, buf: &[u8], addr: A) -> io::Result<usize> {
        self.sys.send_to(buf, addr)
    }

    pub fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        if !is_coroutine() {
            // this can't be nonblocking!!
            try!(self.sys.set_nonblocking(false));
            let ret = try!(self.sys.recv_from(buf));
            try!(self.sys.set_nonblocking(true));;
            return Ok(ret);
        }

        // this is an earlier return try for nonblocking read
        match self.sys.recv_from(buf) {
            Err(err) => {
                if err.kind() != io::ErrorKind::WouldBlock {
                    return Err(err);
                }
            }
            ret @ Ok(..) => {
                return ret;
            }
        }

        let reader = net_impl::UdpRecvFrom::new(self, buf);
        yield_with(&reader);
        reader.done()
    }

    pub fn send(&self, buf: &[u8]) -> io::Result<usize> {
        self.sys.send(buf)
    }

    pub fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.sys.recv(buf)
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

    pub fn broadcast(&self) -> io::Result<bool> {
        self.sys.broadcast()
    }

    pub fn set_broadcast(&self, on: bool) -> io::Result<()> {
        self.sys.set_broadcast(on)
    }

    pub fn multicast_loop_v4(&self) -> io::Result<bool> {
        self.sys.multicast_loop_v4()
    }

    pub fn set_multicast_loop_v4(&self, on: bool) -> io::Result<()> {
        self.sys.set_multicast_loop_v4(on)
    }

    pub fn multicast_ttl_v4(&self) -> io::Result<u32> {
        self.sys.multicast_ttl_v4()
    }

    pub fn set_multicast_ttl_v4(&self, ttl: u32) -> io::Result<()> {
        self.sys.set_multicast_ttl_v4(ttl)
    }

    pub fn multicast_loop_v6(&self) -> io::Result<bool> {
        self.sys.multicast_loop_v6()
    }

    pub fn set_multicast_loop_v6(&self, on: bool) -> io::Result<()> {
        self.sys.set_multicast_loop_v6(on)
    }

    pub fn ttl(&self) -> io::Result<u32> {
        self.sys.ttl()
    }

    pub fn set_ttl(&self, ttl: u32) -> io::Result<()> {
        self.sys.set_ttl(ttl)
    }

    pub fn join_multicast_v4(&self, multiaddr: &Ipv4Addr, interface: &Ipv4Addr) -> io::Result<()> {
        self.sys.join_multicast_v4(multiaddr, interface)
    }

    pub fn join_multicast_v6(&self, multiaddr: &Ipv6Addr, interface: u32) -> io::Result<()> {
        self.sys.join_multicast_v6(multiaddr, interface)
    }

    pub fn leave_multicast_v4(&self, multiaddr: &Ipv4Addr, interface: &Ipv4Addr) -> io::Result<()> {
        self.sys.leave_multicast_v4(multiaddr, interface)
    }

    pub fn leave_multicast_v6(&self, multiaddr: &Ipv6Addr, interface: u32) -> io::Result<()> {
        self.sys.leave_multicast_v6(multiaddr, interface)
    }

    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        self.sys.take_error()
    }
}

// ===== UNIX ext =====
//
//

#[cfg(unix)]
use std::os::unix::io::{IntoRawFd, AsRawFd, FromRawFd, RawFd};

#[cfg(unix)]
impl IntoRawFd for UdpSocket {
    fn into_raw_fd(self) -> RawFd {
        self.sys.into_raw_fd()
    }
}

#[cfg(unix)]
impl AsRawFd for UdpSocket {
    fn as_raw_fd(&self) -> RawFd {
        self.sys.as_raw_fd()
    }
}

#[cfg(unix)]
impl FromRawFd for UdpSocket {
    unsafe fn from_raw_fd(fd: RawFd) -> UdpSocket {
        UdpSocket { sys: FromRawFd::from_raw_fd(fd) }
    }
}

// ===== Windows ext =====
//
//

#[cfg(windows)]
use std::os::windows::io::{IntoRawSocket, AsRawSocket, FromRawSocket, RawSocket};

#[cfg(windows)]
impl IntoRawSocket for UdpSocket {
    fn into_raw_socket(self) -> RawSocket {
        self.sys.into_raw_socket()
    }
}

#[cfg(windows)]
impl AsRawSocket for UdpSocket {
    fn as_raw_socket(&self) -> RawSocket {
        self.sys.as_raw_socket()
    }
}

#[cfg(windows)]
impl FromRawSocket for UdpSocket {
    unsafe fn from_raw_socket(s: RawSocket) -> UdpSocket {
        // TODO: set the time out info here
        // need to set the read/write timeout from sys and sync each other
        UdpSocket::new(FromRawSocket::from_raw_socket(s))
            .unwrap_or_else(|e| panic!("from_raw_socket for UdpSocket, err = {:?}", e))
    }
}
