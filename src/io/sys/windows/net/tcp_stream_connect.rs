use std;
use std::io;
use std::net::{TcpStream, SocketAddr};
use std::os::windows::io::AsRawSocket;
use super::co_io_result;
use super::super::EventData;
use super::super::winapi::*;
use super::super::miow::net::TcpStreamExt;
use scheduler::get_scheduler;
use coroutine::{CoroutineImpl, EventSource};

pub struct TcpStreamConnect {
    io_data: EventData,
    socket: std::net::TcpStream,
    addr: SocketAddr,
}

impl TcpStreamConnect {
    pub fn new(socket: TcpStream, addr: SocketAddr) -> Self {
        TcpStreamConnect {
            io_data: EventData::new(socket.as_raw_socket() as HANDLE),
            socket: socket,
            addr: addr,
        }
    }

    #[inline]
    pub fn done(self) -> io::Result<TcpStream> {
        co_io_result(&self.io_data).map(move |_| self.socket)
    }
}

impl EventSource for TcpStreamConnect {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        // prepare the co first
        self.io_data.co = Some(co);
        // call the overlapped connect API
        co_try!(s, self.io_data.co.take().unwrap(), unsafe {
            self.socket.connect_overlapped(&self.addr, self.io_data.get_overlapped())
        });
    }
}
