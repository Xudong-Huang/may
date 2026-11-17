mod socket_peek;
mod socket_read;
mod socket_write;
mod socket_write_vectored;
mod tcp_listener_accept;
mod tcp_stream_connect;
mod udp_recv_from;
mod udp_send_to;
mod unix_listener_accept;
mod unix_recv_from;
mod unix_send_to;
mod unix_stream_connect;

pub use self::socket_peek::SocketPeek;
pub use self::socket_read::SocketRead;
pub use self::socket_write::SocketWrite;
pub use self::socket_write_vectored::SocketWriteVectored;
pub use self::tcp_listener_accept::TcpListenerAccept;
pub use self::tcp_stream_connect::TcpStreamConnect;
pub use self::udp_recv_from::UdpRecvFrom;
pub use self::udp_send_to::UdpSendTo;
pub use self::unix_listener_accept::UnixListenerAccept;
pub use self::unix_recv_from::UnixRecvFrom;
pub use self::unix_send_to::UnixSendTo;
pub use self::unix_stream_connect::UnixStreamConnect;

use std::io;
use std::net::SocketAddr;

/// Convert the `sockaddr_storage` filled by `RecvMsg` into a std address.
pub(crate) fn socket_addr_from_raw(
    storage: &libc::sockaddr_storage,
    len: libc::socklen_t,
) -> io::Result<SocketAddr> {
    // `try_init` zeroes the output storage and hands us the length; we copy the
    // kernel-filled address into it and let socket2 validate the family.
    let (_, addr) = unsafe {
        socket2::SockAddr::try_init(|out, out_len| {
            std::ptr::copy_nonoverlapping(storage, (*out).view_as::<libc::sockaddr_storage>(), 1);
            *out_len = len;
            Ok(())
        })
    }?;

    addr.as_socket()
        .ok_or_else(|| io::Error::other("io_uring returned a non-IP socket address"))
}
