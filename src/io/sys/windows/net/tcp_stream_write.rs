use std;
use std::io;
use std::time::Duration;
use std::os::windows::io::AsRawSocket;
use super::co_io_result;
use super::super::EventData;
use super::super::winapi::*;
use super::super::miow::net::TcpStreamExt;
use net::TcpStream;
use scheduler::get_scheduler;
use coroutine::{CoroutineImpl, EventSource};

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
