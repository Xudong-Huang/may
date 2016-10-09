use std::io;
use std::os::unix::io::AsRawFd;
use yield_now::get_co_para;
use scheduler::get_scheduler;

mod socket_read;
mod socket_write;
mod tcp_stream_connect;
mod tcp_listener_accpet;
mod udp_send_to;
mod udp_recv_from;

pub use self::socket_read::SocketRead;
pub use self::socket_write::SocketWrite;
pub use self::tcp_stream_connect::TcpStreamConnect;
pub use self::tcp_listener_accpet::TcpListenerAccept;
pub use self::udp_send_to::UdpSendTo;
pub use self::udp_recv_from::UdpRecvFrom;

#[inline]
pub fn add_socket<T: AsRawFd + ?Sized>(t: &T) -> io::Result<()> {
    let s = get_scheduler();
    s.get_selector().add_fd(t.as_raw_fd())
}

// deal with the io result
#[inline]
fn co_io_result() -> io::Result<()> {
    match get_co_para() {
        Some(err) => {
            return Err(err);
        }
        None => {
            return Ok(());
        }
    }
}
