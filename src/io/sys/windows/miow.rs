//! ported from miow crate which is not maintained anymore

use std::os::windows::io::{AsRawHandle, AsRawSocket};
use std::time::Duration;
use std::{io, os::windows::io::RawSocket};

use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Networking::WinSock::*;
use windows_sys::Win32::System::Threading::INFINITE;
use windows_sys::Win32::System::IO::*;

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
        let ret = unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, 0, 0, threads) };
        if ret == 0 {
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
        if ret == 0 {
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
        let len = std::cmp::min(list.len(), <u32>::max_value() as usize) as u32;
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

fn dur2ms(dur: Option<Duration>) -> u32 {
    let dur = match dur {
        Some(dur) => dur,
        None => return INFINITE,
    };
    let ms = dur.as_secs().checked_mul(1_000);
    let ms_extra = dur.subsec_millis();
    ms.and_then(|ms| ms.checked_add(ms_extra as u64))
        .map(|ms| std::cmp::min(u32::max_value() as u64, ms) as u32)
        .unwrap_or(INFINITE - 1)
}

unsafe fn slice2buf(slice: &[u8]) -> WSABUF {
    WSABUF {
        len: std::cmp::min(slice.len(), <u32>::max_value() as usize) as u32,
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

fn cvt_ret(i: i32) -> io::Result<bool> {
    if i == 0 {
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
