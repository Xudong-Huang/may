// extern crate rustc_serialize;
extern crate docopt;
#[macro_use]
extern crate may;
#[macro_use]
extern crate serde_derive;

use std::io::{self, Read, Write};
use std::net::ToSocketAddrs;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use may::coroutine;
use may::net::TcpStream;

use docopt::Docopt;

const VERSION: &str = "0.1.0";

const USAGE: &str = "
Tcp echo client.

Usage:
  echo_client [-t <threads>] [-c <connections>] [-d <time>] [-l <length>] -a <address>
  echo_client (-h | --help)
  echo_client (-v | --version)

Options:
  -h --help         Show this screen.
  -v --version      Show version.
  -t <threads>      number of threads to use [default: 1].
  -l <length>       packet length in bytes [default: 100].
  -c <connections>  concurent connections  [default: 100].
  -d <time>         time to run in seconds [default: 10].
  -a <address>      target address (e.g. 127.0.0.1:8080).
";

#[derive(Debug, Deserialize)]
struct Args {
    flag_a: String,
    flag_c: usize,
    flag_d: usize,
    flag_l: usize,
    flag_t: usize,
    flag_v: bool,
}

macro_rules! t {
    ($e:expr) => {
        match $e {
            Ok(val) => val,
            Err(err) => return println!("call = {:?}\nerr = {:?}", stringify!($e), err),
        }
    };
}

fn main() {
    let args: Args = Docopt::new(USAGE)
        .and_then(|d| d.deserialize())
        .unwrap_or_else(|e| e.exit());

    if args.flag_v {
        return println!("echo_client: {}", VERSION);
    }

    let target_addr: &str = &args.flag_a;
    let test_msg_len = args.flag_l;
    let test_conn_num = args.flag_c;
    let test_seconds = args.flag_d;
    // let io_timeout = 2;

    may::config().set_workers(args.flag_t);

    let stop = AtomicBool::new(false);
    let in_num = AtomicUsize::new(0);
    let out_num = AtomicUsize::new(0);

    let msg = vec![0; test_msg_len];

    let err = io::Error::new(io::ErrorKind::Other, "can't resolve socket addresses");
    let addr = t!(target_addr.to_socket_addrs())
        .fold(Err(err), |prev, addr| prev.or_else(|_| Ok(addr)))
        .unwrap();

    coroutine::scope(|scope| {
        go!(scope, || {
            coroutine::sleep(Duration::from_secs(test_seconds as u64));
            stop.store(true, Ordering::Release);
        });

        // print the result every one second
        go!(scope, || {
            let mut time = 0;
            let mut last_num = 0;
            while !stop.load(Ordering::Relaxed) {
                coroutine::sleep(Duration::from_secs(1));
                time += 1;

                let out_num = out_num.load(Ordering::Relaxed);
                let packets = out_num - last_num;
                last_num = out_num;

                print!(
                    "\r{} Secs, Speed: {} packets/sec,  {} kb/sec\r",
                    time,
                    packets,
                    packets * test_msg_len / 1024
                );
                std::io::stdout().flush().ok();
            }
        });

        for _ in 0..test_conn_num {
            go!(scope, || {
                let mut conn = t!(TcpStream::connect(addr));
                // t!(conn.set_read_timeout(Some(Duration::from_secs(io_timeout))));
                // t!(conn.set_write_timeout(Some(Duration::from_secs(io_timeout))));
                t!(conn.set_nodelay(true));

                let l = msg.len();
                let mut recv = vec![0; l];
                loop {
                    t!(conn.write_all(&msg));
                    out_num.fetch_add(1, Ordering::Relaxed);

                    if stop.load(Ordering::Relaxed) {
                        break;
                    }

                    t!(conn.read_exact(&mut recv));
                    in_num.fetch_add(1, Ordering::Relaxed);

                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                }
            });
        }
    });

    let in_num = in_num.load(Ordering::Relaxed);
    let out_num = out_num.load(Ordering::Relaxed);

    println!(
        "==================Benchmarking: {}==================",
        target_addr
    );
    println!(
        "{} clients, running {} bytes, {} sec.\n",
        test_conn_num, test_msg_len, test_seconds
    );
    println!(
        "Speed: {} request/sec,  {} response/sec, {} kb/sec",
        out_num / test_seconds,
        in_num / test_seconds,
        out_num * test_msg_len / test_seconds / 1024
    );
    println!("Requests: {}", out_num);
    println!("Responses: {}", in_num);
}
