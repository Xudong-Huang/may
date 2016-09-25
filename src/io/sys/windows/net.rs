use std::io;
use std::os::windows::io::AsRawSocket;
use scheduler::get_scheduler;

// register the socket to the system selector
#[inline]
pub fn add_socket<T: AsRawSocket + ?Sized>(t: &T) -> io::Result<()> {
    let s = get_scheduler();
    s.get_selector().add_socket(t)
}
