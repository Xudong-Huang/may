//! Networking primitives
//!
//! Each io object belongs to one coroutine. Do not share one across threads or
//! coroutines and wait on it concurrently; see [`crate::io`] for details.

mod tcp;
mod udp;

pub use self::tcp::{TcpListener, TcpStream};
pub use self::udp::UdpSocket;
