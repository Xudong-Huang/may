use std::io;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Duration;

use super::super::{add_socket, co_io_result, IoData};
use crate::coroutine_impl::{co_get_handle, CoroutineImpl, EventSource};
use crate::io::{CoIo, OptionCell};
use crate::os::unix::net::UnixStream;
use crate::scheduler::get_scheduler;
use crate::yield_now::yield_with;
use libc;
use socket2::{Domain, SockAddr, Socket, Type};

pub struct UnixStreamConnect {
    io_data: OptionCell<IoData>,
    stream: OptionCell<Socket>,
    path: SockAddr,
    is_connected: bool,
}

impl UnixStreamConnect {
    pub fn new<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let path = SockAddr::unix(path)?;
        let socket = Socket::new(Domain::unix(), Type::stream(), None)?;
        // before yield we must set the socket to nonblocking mode and registe to selector
        socket.set_nonblocking(true)?;
        add_socket(&socket).map(|io| UnixStreamConnect {
            io_data: OptionCell::new(io),
            stream: OptionCell::new(socket),
            path,
            is_connected: false,
        })
    }

    #[inline]
    // return ture if it's connected
    pub fn check_connected(&mut self) -> io::Result<bool> {
        // unix connect is some like completion mode
        // we must give the connect request first to the system
        match self.stream.connect(&self.path) {
            Ok(_) => {
                self.is_connected = true;
                Ok(true)
            }
            Err(ref e) if e.raw_os_error() == Some(libc::EINPROGRESS) => Ok(false),
            Err(e) => Err(e),
        }
    }

    pub fn done(&mut self) -> io::Result<UnixStream> {
        fn convert_to_stream(s: &mut UnixStreamConnect) -> UnixStream {
            let stream = s.stream.take().into_unix_stream();
            UnixStream::from_coio(CoIo::from_raw(stream, s.io_data.take()))
        }

        // first check if it's already connected
        if self.is_connected {
            return Ok(convert_to_stream(self));
        }

        loop {
            co_io_result()?;

            // clear the io_flag
            self.io_data.io_flag.store(false, Ordering::Relaxed);

            match self.stream.connect(&self.path) {
                Ok(_) => return Ok(convert_to_stream(self)),
                Err(ref e) if e.raw_os_error() == Some(libc::EINPROGRESS) => {}
                Err(ref e) if e.raw_os_error() == Some(libc::EALREADY) => {}
                Err(ref e) if e.raw_os_error() == Some(libc::EISCONN) => {
                    return Ok(convert_to_stream(self));
                }
                Err(e) => return Err(e),
            }

            if self.io_data.io_flag.swap(false, Ordering::Relaxed) {
                continue;
            }

            // the result is still EINPROGRESS, need to try again
            yield_with(self);
        }
    }
}

impl EventSource for UnixStreamConnect {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let handle = co_get_handle(&co);
        let cancel = handle.get_cancel();
        let io_data = (*self.io_data).clone();

        get_scheduler()
            .get_selector()
            .add_io_timer(&self.io_data, Duration::from_secs(2));
        io_data.co.swap(co, Ordering::Release);

        // there is event, re-run the coroutine
        if io_data.io_flag.load(Ordering::Acquire) {
            return io_data.schedule();
        }

        // register the cancel io data
        cancel.set_io(io_data);
        // re-check the cancel status
        if cancel.is_canceled() {
            unsafe { cancel.cancel() };
        }
    }
}
