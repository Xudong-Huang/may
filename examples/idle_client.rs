extern crate docopt;
#[macro_use]
extern crate may;
#[macro_use]
extern crate serde_derive;

use std::net::ToSocketAddrs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use docopt::Docopt;
use may::coroutine;
use may::net::TcpStream;

const VERSION: &str = "0.1.0";

const USAGE: &str = "
Idle fan-out client: open N connections, hold them, send nothing.

Usage:
  idle_client [-t <threads>] [-c <connections>] [-d <time>] -a <address>
  idle_client (-h | --help)
  idle_client (-v | --version)

Options:
  -h --help         Show this screen.
  -v --version      Show version.
  -t <threads>      number of threads to use [default: 1].
  -c <connections>  concurrent connections [default: 1000].
  -d <time>         hold time in seconds [default: 5].
  -a <address>      target address (e.g. 127.0.0.1:8080).
";

#[derive(Debug, Deserialize)]
struct Args {
    flag_a: String,
    flag_c: usize,
    flag_d: usize,
    flag_t: usize,
    flag_v: bool,
}

fn main() {
    let args: Args = Docopt::new(USAGE)
        .and_then(|d| d.deserialize())
        .unwrap_or_else(|e| e.exit());

    if args.flag_v {
        return println!("idle_client: {VERSION}");
    }

    may::config().set_workers(args.flag_t);

    let target_addr = args.flag_a.clone();
    let addr = target_addr
        .to_socket_addrs()
        .ok()
        .and_then(|mut addrs| addrs.next())
        .unwrap_or_else(|| panic!("can't resolve {target_addr}"));

    let connections = args.flag_c;
    let hold = Duration::from_secs(args.flag_d as u64);
    let connected = Arc::new(AtomicUsize::new(0));

    for _ in 0..connections {
        let connected = connected.clone();
        go!(move || {
            let Ok(stream) = TcpStream::connect(addr) else {
                return;
            };
            let _ = stream.set_nodelay(true);
            connected.fetch_add(1, Ordering::Relaxed);
            // Keep the socket alive and idle for the hold interval.
            coroutine::sleep(hold);
        });
    }

    // The main thread is not a coroutine; sleeping here keeps the process
    // (and every open connection) alive while the server benchmark samples
    // its own CPU.
    std::thread::sleep(hold);
    println!(
        "Idle: {} of {connections} connections held for {} sec",
        connected.load(Ordering::Relaxed),
        args.flag_d
    );
}
