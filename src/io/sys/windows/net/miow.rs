use std::io;

use windows_sys::Win32::Networking::WinSock::{
    WSAGetLastError, SOCKET_ERROR, WSABUF, WSA_IO_PENDING,
};

pub unsafe fn slice2buf(slice: &[u8]) -> WSABUF {
    WSABUF {
        len: std::cmp::min(slice.len(), <u32>::max_value() as usize) as u32,
        buf: slice.as_ptr() as *mut _,
    }
}

fn last_err() -> io::Result<Option<usize>> {
    let err = unsafe { WSAGetLastError() };
    if err == WSA_IO_PENDING as i32 {
        Ok(None)
    } else {
        Err(io::Error::from_raw_os_error(err))
    }
}

pub(crate) fn cvt(i: i32, size: u32) -> io::Result<Option<usize>> {
    if i == SOCKET_ERROR {
        last_err()
    } else {
        Ok(Some(size as usize))
    }
}
