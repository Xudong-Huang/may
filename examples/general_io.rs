#[macro_use]
extern crate may;

use std::io::{self, Read, Write};
use std::time::Duration;

use may::io::CoIo;

// create the io object that can be used in coroutine
// note that we can only access the io object in one thread/coroutine
// this example can't run on windows
// because stdin/stdout can't register on IOCP
fn main() {
    // run every thing in a single thread to verify the async io
    may::config().set_workers(1);

    join!(
        {
            // this will block the whole thread, use Coio instead!!
            // let mut stdin = io::stdin();

            // the CoIo object will not block the thread.
            let mut stdin = CoIo::new(io::stdin()).expect("failed to create stdio");
            loop {
                let mut msg = [0, 4];
                let _n = stdin.read(&mut msg).expect("failed ot read stdio");
                println!("another coroutine, msg={msg:?}");
            }
        },
        {
            let mut stdout = CoIo::new(io::stdout()).expect("failed to create stdout");
            loop {
                writeln!(stdout, "write from coroutine").expect("failed to write");
                may::coroutine::sleep(Duration::from_millis(500));
            }
        }
    );
}
