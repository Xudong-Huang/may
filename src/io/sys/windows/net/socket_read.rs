use std;
use std::io;
use std::time::Duration;
use std::os::windows::io::{IntoRawSocket, FromRawSocket, AsRawSocket};
use super::super::winapi::*;
use super::super::EventData;
use super::super::co_io_result;
use super::super::miow::net::TcpStreamExt;
use cancel::Cancel;
use scheduler::get_scheduler;
use io::cancel::{CancelIoData, CancelIoImpl};
use coroutine::{CoroutineImpl, EventSource, get_cancel_data};

pub struct SocketRead<'a> {
    io_data: EventData,
    buf: &'a mut [u8],
    socket: std::net::TcpStream,
    timeout: Option<Duration>,
    io_cancel: &'static Cancel<CancelIoImpl>,
}

impl<'a> SocketRead<'a> {
    pub fn new<T: AsRawSocket>(s: &T, buf: &'a mut [u8], timeout: Option<Duration>) -> Self {
        let socket = s.as_raw_socket();
        SocketRead {
            io_data: EventData::new(socket as HANDLE),
            buf: buf,
            socket: unsafe { FromRawSocket::from_raw_socket(socket) },
            timeout: timeout,
            io_cancel: get_cancel_data(),
        }
    }

    #[inline]
    pub fn done(self) -> io::Result<usize> {
        // don't close the socket
        self.socket.into_raw_socket();
        co_io_result(&self.io_data)
    }
}

impl<'a> EventSource for SocketRead<'a> {
    fn get_cancel_data(&self) -> Option<&Cancel<CancelIoImpl>> {
        Some(self.io_cancel)
    }

    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        // if the event happened before this there would be something wrong
        // that the timer handle can't be removed in time
        // we must prepare the timer before call the API
        s.get_selector().add_io_timer(&mut self.io_data, self.timeout);
        // prepare the co first
        self.io_data.co = Some(co);

        // call the overlapped read API
        co_try!(s,
                self.io_data.co.take().expect("can't get co"),
                unsafe { self.socket.read_overlapped(self.buf, self.io_data.get_overlapped()) });

        // deal with the cancel
        self.get_cancel_data().map(|cancel| {
            // register the cancel io data
            cancel.set_io(CancelIoData::new(&self.io_data));
            // re-check the cancel status
            if cancel.is_canceled() {
                unsafe { cancel.cancel() };
            }
        });
    }
}
