use std;
use std::io;
use std::time::Duration;
use std::os::windows::io::{AsRawSocket, FromRawSocket, IntoRawSocket};
use miow::net::TcpStreamExt;
use winapi::shared::ntdef::*;
use scheduler::get_scheduler;
use super::super::{co_io_result, EventData};
use coroutine_impl::{CoroutineImpl, EventSource};

pub struct SocketWrite<'a> {
    io_data: EventData,
    buf: &'a [u8],
    socket: std::net::TcpStream,
    timeout: Option<Duration>,
}

impl<'a> SocketWrite<'a> {
    pub fn new<T: AsRawSocket>(s: &T, buf: &'a [u8], timeout: Option<Duration>) -> Self {
        let socket = s.as_raw_socket();
        SocketWrite {
            io_data: EventData::new(socket as HANDLE),
            buf: buf,
            socket: unsafe { FromRawSocket::from_raw_socket(socket) },
            timeout: timeout,
        }
    }

    #[inline]
    pub fn done(self) -> io::Result<usize> {
        // don't close the socket
        self.socket.into_raw_socket();
        co_io_result(&self.io_data)
    }
}

impl<'a> EventSource for SocketWrite<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        s.get_selector()
            .add_io_timer(&mut self.io_data, self.timeout);
        // prepare the co first
        self.io_data.co = Some(co);
        // call the overlapped write API
        co_try!(s, self.io_data.co.take().expect("can't get co"), unsafe {
            self.socket
                .write_overlapped(self.buf, self.io_data.get_overlapped())
        });
    }
}
