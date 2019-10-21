extern crate docopt;
#[macro_use]
extern crate may;
#[macro_use]
extern crate serde_derive;

// use std::time::Duration;
use std::io::{Read, Write};

use docopt::Docopt;
use may::net::{TcpListener, TcpStream};

const VERSION: &str = "0.1.0";

const USAGE: &str = "
Tcp echo server.

Usage:
  echo [-t <threads>] [-p <port>]
  echo (-h | --help)
  echo (-v | --version)

Options:
  -h --help         Show this screen.
  -v --version      Show version.
  -t <threads>      number of threads to use [default: 1].
  -p <address>      port of the server [default: 8080].
";

#[derive(Debug, Deserialize)]
struct Args {
    flag_p: u16,
    flag_t: usize,
    flag_v: bool,
}

macro_rules! t {
    ($e:expr) => {
        match $e {
            Ok(val) => val,
            #[cold]
            Err(err) => return println!("err = {:?}", err),
        }
    };
}

#[inline]
fn handle_client(mut stream: TcpStream) {
    // t!(stream.set_read_timeout(Some(Duration::from_secs(10))));
    // t!(stream.set_write_timeout(Some(Duration::from_secs(10))));
    let mut read = vec![0; 1024 * 16]; // alloc in heap!
    loop {
        let n = t!(stream.read(&mut read));
        if n > 0 {
            t!(stream.write_all(&read[0..n]));
        } else {
            #[cold]
            break;
        }
    }
}

/// simple test: echo hello | nc 127.0.0.1 8080
fn main() {
    let args: Args = Docopt::new(USAGE)
        .and_then(|d| d.deserialize())
        .unwrap_or_else(|e| e.exit());

    if args.flag_v {
        return println!("echo: {}", VERSION);
    }

    let port = args.flag_p;
    let threads = args.flag_t;
    may::config().set_workers(threads);

    may::coroutine::scope(|s| {
        for i in 0..threads {
            go!(s, move || {
                // let listener = TcpListener::bind("127.0.0.1:8080").unwrap();
                let listener = TcpListener::bind(("0.0.0.0", port)).unwrap();

                println!(
                    "Starting tcp echo server on {:?}",
                    listener.local_addr().unwrap(),
                );
                println!("running on thread id {}", i);

                for stream in listener.incoming() {
                    match stream {
                        Ok(s) => {
                            go!(move || handle_client(s));
                        }
                        Err(e) => println!("err = {:?}", e),
                    }
                }
            });
        }
    });
}
