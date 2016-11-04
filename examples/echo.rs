extern crate rustc_serialize;
extern crate coroutine;
extern crate docopt;

// use std::time::Duration;
use std::io::{Read, Write};
use coroutine::net::{TcpListener, TcpStream};
use docopt::Docopt;

const VERSION: &'static str = "0.1.0";

const USAGE: &'static str = "
Tcp echo server.

Usage:
  echo_client [-t <threads>] [-p <port>]
  echo_client (-h | --help)
  echo_client (-v | --version)

Options:
  -h --help         Show this screen.
  -v --version      Show version.
  -t <threads>      number of threads to use [default: 1].
  -p <address>      port of the server [default: 8080].
";

#[derive(Debug, RustcDecodable)]
struct Args {
    flag_p: u16,
    flag_t: usize,
    flag_v: bool,
}

macro_rules! t {
    ($e: expr) => (match $e {
        Ok(val) => val,
        Err(err) => return println!("err = {:?}", err),
    })
}

fn handle_client(mut stream: TcpStream) {
    // t!(stream.set_read_timeout(Some(Duration::from_secs(10))));
    // t!(stream.set_write_timeout(Some(Duration::from_secs(10))));
    let mut read = vec![0; 1024 * 16]; // alloc in heap!
    loop {
        match stream.read(&mut read) {
            Ok(0) => break, // connection was closed
            Ok(n) => t!(stream.write_all(&read[0..n])),
            Err(err) => return println!("err = {:?}", err),
        }
    }
}

/// simple test: echo hello | nc 127.0.0.1 8080
fn main() {
    let args: Args = Docopt::new(USAGE)
        .and_then(|d| d.decode())
        .unwrap_or_else(|e| e.exit());

    if args.flag_v {
        return println!("echo: {}", VERSION);
    }

    let port = args.flag_p;
    let threads = args.flag_t;
    coroutine::scheduler_config().set_io_workers(threads);

    coroutine::spawn(move || {
            // let listener = TcpListener::bind("127.0.0.1:8080").unwrap();
            let listener = TcpListener::bind(("0.0.0.0", port)).unwrap();
            println!("Starting tcp echo server on {:?}\nRunning on {} threads",
                     listener.local_addr().unwrap(),
                     threads);

            for stream in listener.incoming() {
                match stream {
                    Ok(s) => {
                        coroutine::spawn(move || handle_client(s));
                    }
                    Err(e) => println!("err = {:?}", e),
                }
            }
        })
        .join()
        .unwrap();
}
