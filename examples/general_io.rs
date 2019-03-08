#[macro_use]
extern crate may;

use std::io::{self, Read, Write};
use std::time::Duration;

use may::io::CoIo;

// create the io object that can be used in coroutine
// note that we can only access the io ojbect in one thread/coroutin
// this example can't run on windows
// because stdin/stdout can't resiger on IOCP,
// add_socket failed because they are not socket
// add_file failed because they are not opened with overlapped
fn main() {
    // run every thing in a single thread to verify the aysnc io
    may::config().set_io_workers(0).set_workers(1);
    let b_run = true;

    join!(
        {
            // this will block the whole thread, use Coio instead!!
            // let mut stdin = io::stdin();

            // the CoIo object will not block the thread.
            let mut stdin = CoIo::new(io::stdin()).expect("failed to create stdin");
            while b_run {
                let mut msg = [0, 4];
                stdin.read(&mut msg).expect("failed to read stdio");
                println!("another coroutine, msg={:?}", msg);
            }
        },
        {
            let mut stdout = CoIo::new(io::stdout()).expect("failed to create stdout");
            while b_run {
                write!(stdout, "write from coroutine\n").expect("failed to write");
                may::coroutine::sleep(Duration::from_millis(500));
            }
        }
    );
}
