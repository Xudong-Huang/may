use std::io;
use std::net::TcpStream;
use std::time::Duration;
use std::os::windows::io::AsRawSocket;
use super::EventData;
use super::winapi::*;
use super::miow::net::TcpStreamExt;
use yield_now::get_co_para;
use scheduler::get_scheduler;
use coroutine::{CoroutineImpl, EventSource};

// register the socket to the system selector
#[inline]
pub fn add_socket<T: AsRawSocket + ?Sized>(t: &T) -> io::Result<()> {
    let s = get_scheduler();
    s.get_selector().add_socket(t)
}

pub struct TcpStreamRead<'a> {
    io_data: EventData,
    buf: &'a mut [u8],
    socket: &'a TcpStream,
    timeout: Option<Duration>,
}

impl<'a> TcpStreamRead<'a> {
    pub fn new(socket: &'a TcpStream, buf: &'a mut [u8]) -> Self {
        TcpStreamRead {
            io_data: EventData::new(socket.as_raw_socket() as HANDLE),
            buf: buf,
            socket: socket,
            timeout: None,
        }
    }

    #[inline]
    pub fn done(&self) -> io::Result<usize> {
        // deal with the error
        match get_co_para() {
            Some(err) => {
                return Err(err);
            }
            None => {
                return Ok(self.io_data.get_io_size());
            }
        }
    }
}

impl<'a> EventSource for TcpStreamRead<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        // call the overlapped read API
        let ret = co_try!(s, co, unsafe {
            self.socket.read_overlapped(self.buf, self.io_data.get_overlapped())
        });

        // the operation is success, we no need to wait any more
        if ret == true {
            s.schedule_io(co);
            return;
        }

        self.io_data.co = Some(co);
        // register the io operaton
        co_try!(s,
                self.io_data.co.take().unwrap(),
                s.add_io(&mut self.io_data, self.timeout));
    }
}
