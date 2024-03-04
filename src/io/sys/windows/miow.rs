use std::{io, os::windows::io::RawSocket};

use windows_sys::Win32::Networking::WinSock::*;
use windows_sys::Win32::System::IO::OVERLAPPED;

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
