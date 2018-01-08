use std::{self, io};
use std::time::Duration;
use std::os::windows::io::{AsRawSocket, FromRawSocket, IntoRawSocket};
use miow::net::TcpStreamExt;
use winapi::shared::ntdef::*;
use scheduler::get_scheduler;
use io::cancel::CancelIoData;
use sync::delay_drop::DelayDrop;
use super::super::{co_io_result, EventData};
use coroutine_impl::{co_cancel_data, CoroutineImpl, EventSource};

pub struct SocketRead<'a> {
    io_data: EventData,
    buf: &'a mut [u8],
    socket: std::net::TcpStream,
    timeout: Option<Duration>,
    can_drop: DelayDrop,
}

impl<'a> SocketRead<'a> {
    pub fn new<T: AsRawSocket>(s: &T, buf: &'a mut [u8], timeout: Option<Duration>) -> Self {
        let socket = s.as_raw_socket();
        SocketRead {
            io_data: EventData::new(socket as HANDLE),
            buf: buf,
            socket: unsafe { FromRawSocket::from_raw_socket(socket) },
            timeout: timeout,
            can_drop: DelayDrop::new(),
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
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        let cancel = co_cancel_data(&co);
        let _g = self.can_drop.delay_drop();
        // if the event happened before this there would be something wrong
        // that the timer handle can't be removed in time
        // we must prepare the timer before call the API
        s.get_selector()
            .add_io_timer(&mut self.io_data, self.timeout);
        // prepare the co first
        self.io_data.co = Some(co);

        // call the overlapped read API
        co_try!(s, self.io_data.co.take().expect("can't get co"), unsafe {
            self.socket
                .read_overlapped(self.buf, self.io_data.get_overlapped())
        });

        // register the cancel io data
        cancel.set_io(CancelIoData::new(&self.io_data));
        // re-check the cancel status
        if cancel.is_canceled() {
            unsafe { cancel.cancel() };
        }
    }
}
