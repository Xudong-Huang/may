use std::io;
use std::net::ToSocketAddrs;
use std::sync::Arc;
#[cfg(feature = "io_timeout")]
use std::time::Duration;

use io_uring::{opcode, types};
use socket2::SockAddr;

use super::super::event::{co_io_result, EventData};
#[cfg(feature = "io_cancel")]
use crate::coroutine_impl::co_cancel_data;
use crate::coroutine_impl::{is_coroutine, CoroutineImpl, EventSource};
#[cfg(feature = "io_cancel")]
use crate::io::sys::cancel::CancelIoData;
use crate::io::AsIoData;
use crate::net::UdpSocket;
use crate::scheduler::get_scheduler;

pub struct UdpSendTo<'a> {
    event_data: Arc<EventData>,
    _buf: &'a [u8],
    addr: SockAddr,
    iov: libc::iovec,
    msg: Option<libc::msghdr>,
    #[cfg(feature = "io_timeout")]
    timeout: Option<Duration>,
    pub(crate) is_coroutine: bool,
}

impl<'a> UdpSendTo<'a> {
    pub fn new<A: ToSocketAddrs>(
        socket: &'a UdpSocket,
        buf: &'a [u8],
        addr: A,
    ) -> io::Result<Self> {
        let addr = addr
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::other("no socket addresses resolved"))?;

        Ok(UdpSendTo {
            event_data: socket.as_io_data().new_request(),
            _buf: buf,
            addr: addr.into(),
            iov: libc::iovec {
                iov_base: buf.as_ptr() as *mut libc::c_void,
                iov_len: buf.len(),
            },
            msg: None,
            #[cfg(feature = "io_timeout")]
            timeout: socket.write_timeout().unwrap(),
            is_coroutine: is_coroutine(),
        })
    }

    pub fn done(&mut self) -> io::Result<usize> {
        co_io_result(&self.event_data, self.is_coroutine)
    }
}

impl EventSource for UdpSendTo<'_> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        #[cfg(feature = "io_cancel")]
        let cancel = co_cancel_data(&co);

        #[cfg(feature = "io_timeout")]
        if let Some(dur) = self.timeout {
            s.get_selector().add_io_timer(&self.event_data, dur);
        }

        let addr_ptr = self.addr.as_ptr() as *mut libc::c_void;
        let addr_len = self.addr.len();
        let iov_ptr = &mut self.iov as *mut libc::iovec;
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_name = addr_ptr;
        msg.msg_namelen = addr_len;
        msg.msg_iov = iov_ptr;
        msg.msg_iovlen = 1;
        self.msg = Some(msg);

        let msg_ptr = self.msg.as_ref().expect("just stored") as *const libc::msghdr;
        let entry = opcode::SendMsg::new(types::Fd(self.event_data.fd()), msg_ptr).build();

        #[cfg(feature = "io_cancel")]
        let event_data = submit_co!(s, &self.event_data, co, entry);
        #[cfg(not(feature = "io_cancel"))]
        submit_co!(s, &self.event_data, co, entry);

        #[cfg(feature = "io_cancel")]
        {
            cancel.set_io(CancelIoData::new(&event_data));
            if cancel.is_canceled() {
                cancel.cancel();
            }
        }
    }
}
