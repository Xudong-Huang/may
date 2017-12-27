extern crate docopt;
#[macro_use]
extern crate may;
#[macro_use]
extern crate serde_derive;

use may::coroutine;
use may::net::UdpSocket;
// use std::time::Duration;
// use std::io::ErrorKind;

use docopt::Docopt;

const VERSION: &'static str = "0.1.0";

const USAGE: &'static str = "
Udp echo server.

Usage:
  echo_udp [-t <threads>] [-p <port>]
  echo_udp (-h | --help)
  echo_udp (-v | --version)

Options:
  -h --help         Show this screen.
  -v --version      Show version.
  -t <threads>      number of threads to use [default: 1].
  -p <address>      port of the server [default: 30000].
";

#[derive(Debug, Deserialize)]
struct Args {
    flag_p: u16,
    flag_t: usize,
    flag_v: bool,
}

macro_rules! t {
    ($e: expr) => (match $e {
        Ok(val) => val,
        Err(err) => {
            println!("call = {:?}\nerr = {:?}", stringify!($e), err);
            continue;
        }
    })
}

/// simple test: echo hello | nc -u 127.0.0.1 30000
fn main() {
    let args: Args = Docopt::new(USAGE)
        .and_then(|d| d.deserialize())
        .unwrap_or_else(|e| e.exit());

    if args.flag_v {
        return println!("echo_udp: {}", VERSION);
    }

    let port = args.flag_p;
    let threads = args.flag_t;
    may::config().set_io_workers(threads);

    let sock = UdpSocket::bind(("0.0.0.0", port)).unwrap();
    println!(
        "Starting udp echo server on {:?}\nRunning on {} threads",
        sock.local_addr().unwrap(),
        threads
    );

    let mut handlers = Vec::new();
    for _ in 0..threads {
        let sock = t!(sock.try_clone());
        let h: coroutine::JoinHandle<()> = go!(move || {
            let mut buf = vec![0u8; 1024 * 16];
            loop {
                let (len, addr) = t!(sock.recv_from(&mut buf));
                // println!("recv_from: len={:?} addr={:?}", len, addr);
                let mut rest = len;
                while rest > 0 {
                    let i = t!(sock.send_to(&buf[(len - rest)..len], &addr));
                    rest -= i;
                }
            }
        });
        handlers.push(h);
    }

    for j in handlers {
        j.join().unwrap();
    }
}
