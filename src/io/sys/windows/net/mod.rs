use std::io;
use std::os::windows::io::AsRawSocket;
use super::EventData;
use yield_now::get_co_para;
use scheduler::get_scheduler;

mod tcp_stream_read;
mod tcp_stream_write;

pub use self::tcp_stream_read::TcpStreamRead;
pub use self::tcp_stream_write::TcpStreamWrite;


// register the socket to the system selector
#[inline]
pub fn add_socket<T: AsRawSocket + ?Sized>(t: &T) -> io::Result<()> {
    let s = get_scheduler();
    s.get_selector().add_socket(t)
}

// deal with the io result
#[inline]
fn co_io_result(io: &EventData) -> io::Result<usize> {
    match get_co_para() {
        Some(err) => {
            return Err(err);
        }
        None => {
            return Ok(io.get_io_size());
        }
    }
}
