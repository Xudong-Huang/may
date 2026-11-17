use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
#[cfg(feature = "io_timeout")]
use std::time::Duration;

use io_uring::{opcode, types};

use super::super::event::{co_io_result, EventData};
#[cfg(feature = "io_cancel")]
use crate::coroutine_impl::co_cancel_data;
use crate::coroutine_impl::{is_coroutine, CoroutineImpl, EventSource};
#[cfg(feature = "io_cancel")]
use crate::io::sys::cancel::CancelIoData;
use crate::io::AsIoData;
use crate::net::UdpSocket;
use crate::scheduler::get_scheduler;
use crate::sync::delay_drop::DelayDrop;

pub struct UdpRecvFrom<'a> {
    event_data: Arc<EventData>,
    _buf: &'a mut [u8],
    _socket: &'a std::net::UdpSocket,
    name: libc::sockaddr_storage,
    iov: libc::iovec,
    msg: Option<libc::msghdr>,
    #[cfg(feature = "io_timeout")]
    timeout: Option<Duration>,
    can_drop: DelayDrop,
    pub(crate) is_coroutine: bool,
}

impl<'a> UdpRecvFrom<'a> {
    pub fn new(socket: &'a UdpSocket, buf: &'a mut [u8]) -> Self {
        let iov = libc::iovec {
            iov_base: buf.as_mut_ptr() as *mut libc::c_void,
            iov_len: buf.len(),
        };

        UdpRecvFrom {
            event_data: socket.as_io_data().new_request(),
            _buf: buf,
            _socket: socket.inner(),
            name: unsafe { std::mem::zeroed() },
            iov,
            msg: None,
            #[cfg(feature = "io_timeout")]
            timeout: socket.read_timeout().unwrap(),
            can_drop: DelayDrop::new(),
            is_coroutine: is_coroutine(),
        }
    }

    pub fn done(&mut self) -> io::Result<(usize, SocketAddr)> {
        let n = co_io_result(&self.event_data, self.is_coroutine)?;
        let msg = self
            .msg
            .as_ref()
            .ok_or_else(|| io::Error::other("io_uring recv_from msghdr missing"))?;
        let addr = super::socket_addr_from_raw(&self.name, msg.msg_namelen)?;
        Ok((n, addr))
    }
}

impl EventSource for UdpRecvFrom<'_> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        #[cfg(feature = "io_cancel")]
        let cancel = co_cancel_data(&co);
        let _g = self.can_drop.delay_drop();

        #[cfg(feature = "io_timeout")]
        if let Some(dur) = self.timeout {
            s.get_selector().add_io_timer(&self.event_data, dur);
        }

        // The msghdr must point at this frame's name/iov storage and stay
        // valid until the CQE; build it in `subscribe`, not `new`, because the
        // adapter value may still be moved before this call.
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_name = &mut self.name as *mut libc::sockaddr_storage as *mut libc::c_void;
        msg.msg_namelen = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        msg.msg_iov = &mut self.iov;
        msg.msg_iovlen = 1;
        self.msg = Some(msg);

        let msg_ptr = self.msg.as_mut().expect("just stored") as *mut libc::msghdr;
        let entry = opcode::RecvMsg::new(types::Fd(self.event_data.fd()), msg_ptr).build();

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
