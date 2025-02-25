//! ported from miow crate which is not maintained anymore

use std::io;
use std::net::{
    Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6, TcpListener, TcpStream,
};
use std::os::windows::io::{AsRawHandle, AsRawSocket, RawHandle, RawSocket};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Networking::WinSock::*;
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::Threading::INFINITE;
use windows_sys::Win32::System::IO::*;

#[allow(clippy::upper_case_acronyms)]
type BOOL = i32;
const TRUE: BOOL = 1;
const FALSE: BOOL = 0;

/// A handle to an Windows I/O Completion Port.
#[derive(Debug)]
pub struct CompletionPort {
    handle: HANDLE,
}

impl CompletionPort {
    /// Creates a new I/O completion port with the specified concurrency value.
    ///
    /// The number of threads given corresponds to the level of concurrency
    /// allowed for threads associated with this port. Consult the Windows
    /// documentation for more information about this value.
    pub fn new(threads: u32) -> io::Result<CompletionPort> {
        let ret = unsafe {
            CreateIoCompletionPort(INVALID_HANDLE_VALUE, std::ptr::null_mut(), 0, threads)
        };
        if ret.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(CompletionPort { handle: ret })
        }
    }

    /// Associates a new `HANDLE` to this I/O completion port.
    ///
    /// This function will associate the given handle to this port with the
    /// given `token` to be returned in status messages whenever it receives a
    /// notification.
    ///
    /// Any object which is convertible to a `HANDLE` via the `AsRawHandle`
    /// trait can be provided to this function, such as `std::fs::File` and
    /// friends.
    #[allow(dead_code)]
    pub fn add_handle<T: AsRawHandle + ?Sized>(&self, token: usize, t: &T) -> io::Result<()> {
        self._add(token, t.as_raw_handle() as HANDLE)
    }

    /// Associates a new `SOCKET` to this I/O completion port.
    ///
    /// This function will associate the given socket to this port with the
    /// given `token` to be returned in status messages whenever it receives a
    /// notification.
    ///
    /// Any object which is convertible to a `SOCKET` via the `AsRawSocket`
    /// trait can be provided to this function, such as `std::net::TcpStream`
    /// and friends.
    pub fn add_socket<T: AsRawSocket + ?Sized>(&self, token: usize, t: &T) -> io::Result<()> {
        self._add(token, t.as_raw_socket() as HANDLE)
    }

    fn _add(&self, token: usize, handle: HANDLE) -> io::Result<()> {
        assert_eq!(std::mem::size_of_val(&token), std::mem::size_of::<usize>());
        let ret = unsafe { CreateIoCompletionPort(handle, self.handle, token, 0) };
        if ret.is_null() {
            Err(io::Error::last_os_error())
        } else {
            debug_assert_eq!(ret, self.handle);
            Ok(())
        }
    }

    /// Dequeue a completion status from this I/O completion port.
    ///
    /// This function will associate the calling thread with this completion
    /// port and then wait for a status message to become available. The precise
    /// semantics on when this function returns depends on the concurrency value
    /// specified when the port was created.
    ///
    /// A timeout can optionally be specified to this function. If `None` is
    /// provided this function will not time out, and otherwise it will time out
    /// after the specified duration has passed.
    ///
    /// On success this will return the status message which was dequeued from
    /// this completion port.
    #[allow(dead_code)]
    pub fn get(&self, timeout: Option<Duration>) -> io::Result<CompletionStatus> {
        let mut bytes = 0;
        let mut token = 0;
        let mut overlapped = std::ptr::null_mut();
        let timeout = dur2ms(timeout);
        let ret = unsafe {
            GetQueuedCompletionStatus(
                self.handle,
                &mut bytes,
                &mut token,
                &mut overlapped,
                timeout,
            )
        };
        cvt(ret, 0).map(|_| {
            CompletionStatus(OVERLAPPED_ENTRY {
                dwNumberOfBytesTransferred: bytes,
                lpCompletionKey: token,
                lpOverlapped: overlapped,
                Internal: 0,
            })
        })
    }

    /// Dequeues a number of completion statuses from this I/O completion port.
    ///
    /// This function is the same as `get` except that it may return more than
    /// one status. A buffer of "zero" statuses is provided (the contents are
    /// not read) and then on success this function will return a sub-slice of
    /// statuses which represent those which were dequeued from this port. This
    /// function does not wait to fill up the entire list of statuses provided.
    ///
    /// Like with `get`, a timeout may be specified for this operation.
    pub fn get_many<'a>(
        &self,
        list: &'a mut [CompletionStatus],
        timeout: Option<Duration>,
    ) -> io::Result<&'a mut [CompletionStatus]> {
        debug_assert_eq!(
            std::mem::size_of::<CompletionStatus>(),
            std::mem::size_of::<OVERLAPPED_ENTRY>()
        );
        let mut removed = 0;
        let timeout = dur2ms(timeout);
        let len = std::cmp::min(list.len(), u32::MAX as usize) as u32;
        let ret = unsafe {
            GetQueuedCompletionStatusEx(
                self.handle,
                list.as_ptr() as *mut _,
                len,
                &mut removed,
                timeout,
                0,
            )
        };
        match cvt_ret(ret) {
            Ok(_) => Ok(&mut list[..removed as usize]),
            Err(e) => Err(e),
        }
    }

    /// Posts a new completion status onto this I/O completion port.
    ///
    /// This function will post the given status, with custom parameters, to the
    /// port. Threads blocked in `get` or `get_many` will eventually receive
    /// this status.
    pub fn post(&self, status: CompletionStatus) -> io::Result<()> {
        let ret = unsafe {
            PostQueuedCompletionStatus(
                self.handle,
                status.0.dwNumberOfBytesTransferred,
                status.0.lpCompletionKey,
                status.0.lpOverlapped,
            )
        };
        cvt_ret(ret).map(|_| ())
    }
}

// impl AsRawHandle for CompletionPort {
//     fn as_raw_handle(&self) -> RawHandle {
//         self.handle.raw() as RawHandle
//     }
// }

// impl FromRawHandle for CompletionPort {
//     unsafe fn from_raw_handle(handle: RawHandle) -> CompletionPort {
//         CompletionPort {
//             handle: Handle::new(handle as HANDLE),
//         }
//     }
// }

// impl IntoRawHandle for CompletionPort {
//     fn into_raw_handle(self) -> RawHandle {
//         self.handle.into_raw() as RawHandle
//     }
// }

/// A status message received from an I/O completion port.
///
/// These statuses can be created via the `new` or `empty` constructors and then
/// provided to a completion port, or they are read out of a completion port.
/// The fields of each status are read through its accessor methods.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct CompletionStatus(OVERLAPPED_ENTRY);

impl std::fmt::Debug for CompletionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "CompletionStatus(OVERLAPPED_ENTRY)")
    }
}

unsafe impl Send for CompletionStatus {}
unsafe impl Sync for CompletionStatus {}

impl CompletionStatus {
    /// Creates a new completion status with the provided parameters.
    ///
    /// This function is useful when creating a status to send to a port with
    /// the `post` method. The parameters are opaquely passed through and not
    /// interpreted by the system at all.
    pub fn new(bytes: u32, token: usize, overlapped: *mut OVERLAPPED) -> CompletionStatus {
        assert_eq!(std::mem::size_of_val(&token), std::mem::size_of::<usize>());
        CompletionStatus(OVERLAPPED_ENTRY {
            dwNumberOfBytesTransferred: bytes,
            lpCompletionKey: token,
            lpOverlapped: overlapped,
            Internal: 0,
        })
    }

    /// Creates a new borrowed completion status from the borrowed
    /// `OVERLAPPED_ENTRY` argument provided.
    ///
    /// This method will wrap the `OVERLAPPED_ENTRY` in a `CompletionStatus`,
    /// returning the wrapped structure.
    #[allow(dead_code)]
    pub fn from_entry(entry: &OVERLAPPED_ENTRY) -> &CompletionStatus {
        // Safety: CompletionStatus is repr(transparent) w/ OVERLAPPED_ENTRY, so
        // a reference to one is guaranteed to be layout compatible with the
        // reference to another.
        unsafe { &*(entry as *const _ as *const _) }
    }

    /// Creates a new "zero" completion status.
    ///
    /// This function is useful when creating a stack buffer or vector of
    /// completion statuses to be passed to the `get_many` function.
    #[allow(dead_code)]
    pub fn zero() -> CompletionStatus {
        CompletionStatus::new(0, 0, std::ptr::null_mut())
    }

    /// Returns the number of bytes that were transferred for the I/O operation
    /// associated with this completion status.
    #[allow(dead_code)]
    pub fn bytes_transferred(&self) -> u32 {
        self.0.dwNumberOfBytesTransferred
    }

    /// Returns the completion key value associated with the file handle whose
    /// I/O operation has completed.
    ///
    /// A completion key is a per-handle key that is specified when it is added
    /// to an I/O completion port via `add_handle` or `add_socket`.
    #[allow(dead_code)]
    pub fn token(&self) -> usize {
        self.0.lpCompletionKey
    }

    /// Returns a pointer to the `Overlapped` structure that was specified when
    /// the I/O operation was started.
    pub fn overlapped(&self) -> *mut OVERLAPPED {
        self.0.lpOverlapped
    }

    /// Returns a pointer to the internal `OVERLAPPED_ENTRY` object.
    #[allow(dead_code)]
    pub fn entry(&self) -> &OVERLAPPED_ENTRY {
        &self.0
    }
}

struct WsaExtension {
    guid: GUID,
    val: AtomicUsize,
}

impl WsaExtension {
    fn get(&self, socket: SOCKET) -> io::Result<usize> {
        let prev = self.val.load(Ordering::SeqCst);
        if prev != 0 && !cfg!(debug_assertions) {
            return Ok(prev);
        }
        let mut ret = 0;
        let mut bytes = 0;

        // https://github.com/microsoft/win32metadata/issues/671
        const SIO_GET_EXTENSION_FUNCTION_POINTER: u32 = 33_5544_3206u32;

        let r = unsafe {
            WSAIoctl(
                socket,
                SIO_GET_EXTENSION_FUNCTION_POINTER,
                &self.guid as *const _ as *mut _,
                std::mem::size_of_val(&self.guid) as u32,
                &mut ret as *mut _ as *mut _,
                std::mem::size_of_val(&ret) as u32,
                &mut bytes,
                std::ptr::null_mut(),
                None,
            )
        };
        cvt(r, 0).map(|_| {
            debug_assert_eq!(bytes as usize, std::mem::size_of_val(&ret));
            debug_assert!(prev == 0 || prev == ret);
            self.val.store(ret, Ordering::SeqCst);
            ret
        })
    }
}

fn dur2ms(dur: Option<Duration>) -> u32 {
    let dur = match dur {
        Some(dur) => dur,
        None => return INFINITE,
    };
    let ms = dur.as_secs().checked_mul(1_000);
    let ms_extra = dur.subsec_millis();
    ms.and_then(|ms| ms.checked_add(ms_extra as u64))
        .map(|ms| std::cmp::min(u32::MAX as u64, ms) as u32)
        .unwrap_or(INFINITE - 1)
}

unsafe fn slice2buf(slice: &[u8]) -> WSABUF {
    WSABUF {
        len: std::cmp::min(slice.len(), u32::MAX as usize) as u32,
        buf: slice.as_ptr() as *mut _,
    }
}

fn last_err() -> io::Result<Option<usize>> {
    let err = unsafe { WSAGetLastError() };
    if err == WSA_IO_PENDING {
        Ok(None)
    } else {
        Err(io::Error::from_raw_os_error(err))
    }
}

fn cvt_ret(i: BOOL) -> io::Result<bool> {
    if i == FALSE {
        Err(io::Error::last_os_error())
    } else {
        Ok(i != 0)
    }
}

fn cvt(i: i32, size: u32) -> io::Result<Option<usize>> {
    if i == SOCKET_ERROR {
        last_err()
    } else {
        Ok(Some(size as usize))
    }
}

/// A type with the same memory layout as `SOCKADDR`. Used in converting Rust level
/// SocketAddr* types into their system representation. The benefit of this specific
/// type over using `SOCKADDR_STORAGE` is that this type is exactly as large as it
/// needs to be and not a lot larger. And it can be initialized cleaner from Rust.
#[repr(C)]
pub(crate) union SocketAddrCRepr {
    v4: SOCKADDR_IN,
    v6: SOCKADDR_IN6,
}

impl SocketAddrCRepr {
    pub(crate) fn as_ptr(&self) -> *const SOCKADDR {
        self as *const _ as *const SOCKADDR
    }
}

fn socket_addr_to_ptrs(addr: &SocketAddr) -> (SocketAddrCRepr, i32) {
    match *addr {
        SocketAddr::V4(ref a) => {
            let sin_addr = IN_ADDR {
                S_un: IN_ADDR_0 {
                    S_addr: u32::from_ne_bytes(a.ip().octets()),
                },
            };

            let sockaddr_in = SOCKADDR_IN {
                sin_family: AF_INET as _,
                sin_port: a.port().to_be(),
                sin_addr,
                sin_zero: [0; 8],
            };

            let sockaddr = SocketAddrCRepr { v4: sockaddr_in };
            (sockaddr, std::mem::size_of::<SOCKADDR_IN>() as i32)
        }
        SocketAddr::V6(ref a) => {
            let sockaddr_in6 = SOCKADDR_IN6 {
                sin6_family: AF_INET6 as _,
                sin6_port: a.port().to_be(),
                sin6_addr: IN6_ADDR {
                    u: IN6_ADDR_0 {
                        Byte: a.ip().octets(),
                    },
                },
                sin6_flowinfo: a.flowinfo(),
                Anonymous: SOCKADDR_IN6_0 {
                    sin6_scope_id: a.scope_id(),
                },
            };

            let sockaddr = SocketAddrCRepr { v6: sockaddr_in6 };
            (sockaddr, std::mem::size_of::<SOCKADDR_IN6>() as i32)
        }
    }
}

#[doc(hidden)]
trait NetInt {
    fn from_be(i: Self) -> Self;
    #[allow(dead_code)]
    fn to_be(&self) -> Self;
}
macro_rules! doit {
    ($($t:ident)*) => ($(impl NetInt for $t {
        fn from_be(i: Self) -> Self { <$t>::from_be(i) }
        fn to_be(&self) -> Self { <$t>::to_be(*self) }
    })*)
}
doit! { i8 i16 i32 i64 isize u8 u16 u32 u64 usize }

// fn hton<I: NetInt>(i: I) -> I { i.to_be() }
fn ntoh<I: NetInt>(i: I) -> I {
    I::from_be(i)
}

unsafe fn ptrs_to_socket_addr(ptr: *const SOCKADDR, len: i32) -> Option<SocketAddr> {
    if (len as usize) < std::mem::size_of::<i32>() {
        return None;
    }
    match (*ptr).sa_family as _ {
        AF_INET if len as usize >= std::mem::size_of::<SOCKADDR_IN>() => {
            let b = &*(ptr as *const SOCKADDR_IN);
            let ip = ntoh(b.sin_addr.S_un.S_addr);
            let ip = Ipv4Addr::new(
                (ip >> 24) as u8,
                (ip >> 16) as u8,
                (ip >> 8) as u8,
                ip as u8,
            );
            Some(SocketAddr::V4(SocketAddrV4::new(ip, ntoh(b.sin_port))))
        }
        AF_INET6 if len as usize >= std::mem::size_of::<SOCKADDR_IN6>() => {
            let b = &*(ptr as *const SOCKADDR_IN6);
            let arr = &b.sin6_addr.u.Byte;
            let ip = Ipv6Addr::new(
                ((arr[0] as u16) << 8) | (arr[1] as u16),
                ((arr[2] as u16) << 8) | (arr[3] as u16),
                ((arr[4] as u16) << 8) | (arr[5] as u16),
                ((arr[6] as u16) << 8) | (arr[7] as u16),
                ((arr[8] as u16) << 8) | (arr[9] as u16),
                ((arr[10] as u16) << 8) | (arr[11] as u16),
                ((arr[12] as u16) << 8) | (arr[13] as u16),
                ((arr[14] as u16) << 8) | (arr[15] as u16),
            );
            let addr = SocketAddrV6::new(
                ip,
                ntoh(b.sin6_port),
                ntoh(b.sin6_flowinfo),
                ntoh(b.Anonymous.sin6_scope_id),
            );
            Some(SocketAddr::V6(addr))
        }
        _ => None,
    }
}

pub unsafe fn socket_read(
    socket: RawSocket,
    buf: &mut [u8],
    flags: i32,
    overlapped: *mut OVERLAPPED,
) -> io::Result<Option<usize>> {
    let buf = slice2buf(buf);
    let mut bytes_read: u32 = 0;
    let mut flags = flags as u32;
    let r = WSARecv(
        socket as SOCKET,
        &buf,
        1,
        &mut bytes_read,
        &mut flags,
        overlapped,
        None,
    );
    cvt(r, bytes_read)
}

pub unsafe fn socket_write(
    socket: RawSocket,
    buf: &[u8],
    overlapped: *mut OVERLAPPED,
) -> io::Result<Option<usize>> {
    let buf = slice2buf(buf);
    let mut bytes_written = 0;
    // Note here that we capture the number of bytes written. The
    // documentation on MSDN, however, states:
    //
    // > Use NULL for this parameter if the lpOverlapped parameter is not
    // > NULL to avoid potentially erroneous results. This parameter can be
    // > NULL only if the lpOverlapped parameter is not NULL.
    //
    // If we're not passing a null overlapped pointer here, then why are we
    // then capturing the number of bytes! Well so it turns out that this is
    // clearly faster to learn the bytes here rather than later calling
    // `WSAGetOverlappedResult`, and in practice almost all implementations
    // use this anyway [1].
    //
    // As a result we use this to and report back the result.
    //
    // [1]: https://github.com/carllerche/mio/pull/520#issuecomment-273983823
    let r = WSASend(
        socket as SOCKET,
        &buf,
        1,
        &mut bytes_written,
        0,
        overlapped,
        None,
    );
    cvt(r, bytes_written)
}

pub unsafe fn connect_overlapped(
    socket: RawSocket,
    addr: &SocketAddr,
    buf: &[u8],
    overlapped: *mut OVERLAPPED,
) -> io::Result<Option<usize>> {
    static CONNECTEX: WsaExtension = WsaExtension {
        guid: GUID {
            data1: 0x25a207b9,
            data2: 0xddf3,
            data3: 0x4660,
            data4: [0x8e, 0xe9, 0x76, 0xe5, 0x8c, 0x74, 0x06, 0x3e],
        },
        val: AtomicUsize::new(0),
    };

    let socket = socket as SOCKET;

    let ptr = CONNECTEX.get(socket)?;
    assert!(ptr != 0);
    let connect_ex = std::mem::transmute::<usize, LPFN_CONNECTEX>(ptr).unwrap();

    let (addr_buf, addr_len) = socket_addr_to_ptrs(addr);
    let mut bytes_sent: u32 = 0;
    let r = connect_ex(
        socket,
        addr_buf.as_ptr(),
        addr_len,
        buf.as_ptr() as *mut _,
        buf.len() as u32,
        &mut bytes_sent,
        overlapped,
    );
    if r == TRUE {
        Ok(Some(bytes_sent as usize))
    } else {
        last_err()
    }
}

pub fn connect_complete(socket: RawSocket) -> io::Result<()> {
    const SO_UPDATE_CONNECT_CONTEXT: i32 = 0x7010;
    let result = unsafe {
        setsockopt(
            socket as SOCKET,
            SOL_SOCKET as _,
            SO_UPDATE_CONNECT_CONTEXT,
            std::ptr::null_mut(),
            0,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

static GETACCEPTEXSOCKADDRS: WsaExtension = WsaExtension {
    guid: GUID {
        data1: 0xb5367df2,
        data2: 0xcbac,
        data3: 0x11cf,
        data4: [0x95, 0xca, 0x00, 0x80, 0x5f, 0x48, 0xa1, 0x92],
    },
    val: AtomicUsize::new(0),
};

/// A type to represent a buffer in which an accepted socket's address will be
/// stored.
///
/// This type is used with the `accept_overlapped` method on the
/// `TcpListenerExt` trait to provide space for the overlapped I/O operation to
/// fill in the socket addresses upon completion.
#[repr(C)]
pub struct AcceptAddrsBuf {
    // For AcceptEx we've got the restriction that the addresses passed in that
    // buffer need to be at least 16 bytes more than the maximum address length
    // for the protocol in question, so add some extra here and there
    local: SOCKADDR_STORAGE,
    _pad1: [u8; 16],
    remote: SOCKADDR_STORAGE,
    _pad2: [u8; 16],
}

impl AcceptAddrsBuf {
    /// Creates a new blank buffer ready to be passed to a call to
    /// `accept_overlapped`.
    pub fn new() -> AcceptAddrsBuf {
        unsafe { std::mem::zeroed() }
    }

    /// Parses the data contained in this address buffer, returning the parsed
    /// result if successful.
    ///
    /// This function can be called after a call to `accept_overlapped` has
    /// succeeded to parse out the data that was written in.
    pub fn parse(&self, socket: &TcpListener) -> io::Result<AcceptAddrs> {
        let mut ret = AcceptAddrs {
            local: std::ptr::null_mut(),
            local_len: 0,
            remote: std::ptr::null_mut(),
            remote_len: 0,
            _data: self,
        };
        let ptr = GETACCEPTEXSOCKADDRS.get(socket.as_raw_socket() as SOCKET)?;
        assert!(ptr != 0);
        unsafe {
            let get_sockaddrs =
                std::mem::transmute::<usize, LPFN_GETACCEPTEXSOCKADDRS>(ptr).unwrap();
            let (a, b, c, d) = self.args();
            get_sockaddrs(
                a,
                b,
                c,
                d,
                &mut ret.local,
                &mut ret.local_len,
                &mut ret.remote,
                &mut ret.remote_len,
            );
            Ok(ret)
        }
    }

    fn args(&self) -> (*mut std::ffi::c_void, u32, u32, u32) {
        let remote_offset = std::mem::offset_of!(AcceptAddrsBuf, remote);
        (
            self as *const _ as *mut _,
            0,
            remote_offset as u32,
            (std::mem::size_of_val(self) - remote_offset) as u32,
        )
    }
}

/// The parsed return value of `AcceptAddrsBuf`.
pub struct AcceptAddrs<'a> {
    local: *mut SOCKADDR,
    local_len: i32,
    remote: *mut SOCKADDR,
    remote_len: i32,
    _data: &'a AcceptAddrsBuf,
}

impl AcceptAddrs<'_> {
    /// Returns the local socket address contained in this buffer.
    #[allow(dead_code)]
    pub fn local(&self) -> Option<SocketAddr> {
        unsafe { ptrs_to_socket_addr(self.local, self.local_len) }
    }

    /// Returns the remote socket address contained in this buffer.
    pub fn remote(&self) -> Option<SocketAddr> {
        unsafe { ptrs_to_socket_addr(self.remote, self.remote_len) }
    }
}

pub unsafe fn accept_overlapped(
    me: RawSocket,
    socket: &TcpStream,
    addrs: &mut AcceptAddrsBuf,
    overlapped: *mut OVERLAPPED,
) -> io::Result<bool> {
    static ACCEPTEX: WsaExtension = WsaExtension {
        guid: GUID {
            data1: 0xb5367df1,
            data2: 0xcbac,
            data3: 0x11cf,
            data4: [0x95, 0xca, 0x00, 0x80, 0x5f, 0x48, 0xa1, 0x92],
        },
        val: AtomicUsize::new(0),
    };

    let ptr = ACCEPTEX.get(me as SOCKET)?;
    assert!(ptr != 0);
    let accept_ex = std::mem::transmute::<usize, LPFN_ACCEPTEX>(ptr).unwrap();

    let mut bytes = 0;
    let (a, b, c, d) = (*addrs).args();
    let r = accept_ex(
        me as SOCKET,
        socket.as_raw_socket() as SOCKET,
        a,
        b,
        c,
        d,
        &mut bytes,
        overlapped,
    );
    let succeeded = if r == TRUE {
        true
    } else {
        last_err()?;
        false
    };
    Ok(succeeded)
}

pub fn accept_complete(me: RawSocket, socket: &TcpStream) -> io::Result<()> {
    const SO_UPDATE_ACCEPT_CONTEXT: i32 = 0x700B;
    let result = unsafe {
        setsockopt(
            socket.as_raw_socket() as SOCKET,
            SOL_SOCKET as _,
            SO_UPDATE_ACCEPT_CONTEXT,
            &me as *const _ as *mut _,
            std::mem::size_of_val(&me) as i32,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// A type to represent a buffer in which a socket address will be stored.
///
/// This type is used with the `recv_from_overlapped` function on the
/// `UdpSocketExt` trait to provide space for the overlapped I/O operation to
/// fill in the address upon completion.
#[derive(Clone, Copy)]
pub struct SocketAddrBuf {
    buf: SOCKADDR_STORAGE,
    len: i32,
}

impl SocketAddrBuf {
    /// Creates a new blank socket address buffer.
    ///
    /// This should be used before a call to `recv_from_overlapped` overlapped
    /// to create an instance to pass down.
    pub fn new() -> SocketAddrBuf {
        SocketAddrBuf {
            buf: unsafe { std::mem::zeroed() },
            len: std::mem::size_of::<SOCKADDR_STORAGE>() as i32,
        }
    }

    /// Parses this buffer to return a standard socket address.
    ///
    /// This function should be called after the buffer has been filled in with
    /// a call to `recv_from_overlapped` being completed. It will interpret the
    /// address filled in and return the standard socket address type.
    ///
    /// If an error is encountered then `None` is returned.
    pub fn get_socket_addr(&self) -> Option<SocketAddr> {
        let ptr = (&self.buf as *const SOCKADDR_STORAGE).cast();
        unsafe { ptrs_to_socket_addr(ptr, self.len) }
    }
}

pub unsafe fn recv_from_overlapped(
    socket: RawSocket,
    buf: &mut [u8],
    addr: *mut SocketAddrBuf,
    overlapped: *mut OVERLAPPED,
) -> io::Result<Option<usize>> {
    let buf = slice2buf(buf);
    let mut flags = 0;
    let mut received_bytes: u32 = 0;
    let r = WSARecvFrom(
        socket as SOCKET,
        &buf,
        1,
        &mut received_bytes,
        &mut flags,
        &mut (*addr).buf as *mut _ as *mut _,
        &mut (*addr).len,
        overlapped,
        None,
    );
    cvt(r, received_bytes)
}

pub unsafe fn send_to_overlapped(
    socket: RawSocket,
    buf: &[u8],
    addr: &SocketAddr,
    overlapped: *mut OVERLAPPED,
) -> io::Result<Option<usize>> {
    let (addr_buf, addr_len) = socket_addr_to_ptrs(addr);
    let buf = slice2buf(buf);
    let mut sent_bytes = 0;
    let r = WSASendTo(
        socket as SOCKET,
        &buf,
        1,
        &mut sent_bytes,
        0,
        addr_buf.as_ptr() as *const _,
        addr_len,
        overlapped,
        None,
    );
    cvt(r, sent_bytes)
}

pub unsafe fn pipe_read_overlapped(
    handle: RawHandle,
    buf: &mut [u8],
    overlapped: *mut OVERLAPPED,
) -> io::Result<Option<usize>> {
    let wait = FALSE;
    let len = std::cmp::min(buf.len(), u32::MAX as usize) as u32;
    let res = cvt_ret({
        ReadFile(
            handle as HANDLE,
            buf.as_mut_ptr() as *mut _,
            len,
            std::ptr::null_mut(),
            overlapped,
        )
    });
    match res {
        Ok(_) => (),
        Err(ref e) if e.raw_os_error() == Some(ERROR_IO_PENDING as i32) => (),
        Err(e) => return Err(e),
    }

    let mut bytes = 0;
    let res = cvt_ret(GetOverlappedResult(
        handle as HANDLE,
        overlapped,
        &mut bytes,
        wait,
    ));
    match res {
        Ok(_) => Ok(Some(bytes as usize)),
        Err(ref e) if e.raw_os_error() == Some(ERROR_IO_INCOMPLETE as i32) && wait == FALSE => {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

pub unsafe fn pipe_write_overlapped(
    handle: RawHandle,
    buf: &[u8],
    overlapped: *mut OVERLAPPED,
) -> io::Result<Option<usize>> {
    let wait = FALSE;
    let len = std::cmp::min(buf.len(), u32::MAX as usize) as u32;
    let res = cvt_ret({
        WriteFile(
            handle as HANDLE,
            buf.as_ptr() as *const _,
            len,
            std::ptr::null_mut(),
            overlapped,
        )
    });
    match res {
        Ok(_) => (),
        Err(ref e) if e.raw_os_error() == Some(ERROR_IO_PENDING as i32) => (),
        Err(e) => return Err(e),
    }

    let mut bytes = 0;
    let res = cvt_ret(GetOverlappedResult(
        handle as HANDLE,
        overlapped,
        &mut bytes,
        wait,
    ));
    match res {
        Ok(_) => Ok(Some(bytes as usize)),
        Err(ref e) if e.raw_os_error() == Some(ERROR_IO_INCOMPLETE as i32) && wait == FALSE => {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}
