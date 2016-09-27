use std;
use std::io;
use std::time::Duration;
use std::os::windows::io::AsRawSocket;
use super::EventData;
use super::winapi::*;
use super::miow::net::TcpStreamExt;
use net::TcpStream;
use yield_now::get_co_para;
use scheduler::get_scheduler;
use coroutine::{CoroutineImpl, EventSource};

// register the socket to the system selector
#[inline]
pub fn add_socket<T: AsRawSocket + ?Sized>(t: &T) -> io::Result<()> {
    let s = get_scheduler();
    s.get_selector().add_socket(t)
}

// deal with the io result
#[inline]
fn co_io_result(io: &EventData) -> io::Result<usize> {
    match get_co_para() {
        Some(err) => {
            return Err(err);
        }
        None => {
            return Ok(io.get_io_size());
        }
    }
}

/// /////////////////////////////////////////////////////////////////////////////
/// TcpStreamRead
/// ////////////////////////////////////////////////////////////////////////////

pub struct TcpStreamRead<'a> {
    io_data: EventData,
    buf: &'a mut [u8],
    socket: &'a std::net::TcpStream,
    timeout: Option<Duration>,
}

impl<'a> TcpStreamRead<'a> {
    pub fn new(socket: &'a TcpStream, buf: &'a mut [u8]) -> Self {
        TcpStreamRead {
            io_data: EventData::new(socket.as_raw_socket() as HANDLE),
            buf: buf,
            socket: socket.inner(),
            timeout: socket.read_timeout().unwrap(),
        }
    }

    #[inline]
    pub fn done(&self) -> io::Result<usize> {
        co_io_result(&self.io_data)
    }
}

impl<'a> EventSource for TcpStreamRead<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        // prepare the co first
        self.io_data.co = Some(co);
        // call the overlapped read API
        let ret = co_try!(s, self.io_data.co.take().unwrap(), unsafe {
            self.socket.read_overlapped(self.buf, self.io_data.get_overlapped())
        });

        // the operation is success, we no need to wait any more
        if ret == true {
            // the done function would contain the actual io size
            // just let the iocp schedule the coroutine
            // s.schedule_io(co);
            return;
        }

        // register the io operaton
        co_try!(s,
                self.io_data.co.take().unwrap(),
                s.add_io(&mut self.io_data, self.timeout));
    }
}

/// /////////////////////////////////////////////////////////////////////////////
/// TcpStreamWrite
/// ////////////////////////////////////////////////////////////////////////////

pub struct TcpStreamWrite<'a> {
    io_data: EventData,
    buf: &'a [u8],
    socket: &'a std::net::TcpStream,
    timeout: Option<Duration>,
}

impl<'a> TcpStreamWrite<'a> {
    pub fn new(socket: &'a TcpStream, buf: &'a [u8]) -> Self {
        TcpStreamWrite {
            io_data: EventData::new(socket.as_raw_socket() as HANDLE),
            buf: buf,
            socket: socket.inner(),
            timeout: socket.write_timeout().unwrap(),
        }
    }

    #[inline]
    pub fn done(&self) -> io::Result<usize> {
        co_io_result(&self.io_data)
    }
}

impl<'a> EventSource for TcpStreamWrite<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        // prepare the co first
        self.io_data.co = Some(co);
        // call the overlapped write API
        let ret = co_try!(s, self.io_data.co.take().unwrap(), unsafe {
            self.socket.write_overlapped(self.buf, self.io_data.get_overlapped())
        });

        // the operation is success, we no need to wait any more
        if ret == true {
            return;
        }

        // register the io operaton
        co_try!(s,
                self.io_data.co.take().unwrap(),
                s.add_io(&mut self.io_data, self.timeout));
    }
}
