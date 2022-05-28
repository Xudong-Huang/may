extern crate docopt;
#[macro_use]
extern crate may;
#[macro_use]
extern crate serde_derive;

use std::io::{self, Write};
use std::net::ToSocketAddrs;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use docopt::Docopt;
use may::coroutine;
use may::net::UdpSocket;

const VERSION: &str = "0.1.0";

const USAGE: &str = "
Udp echo client.

Usage:
  echo_upd_client [-d <time>] [-c <connections>] [-t <threads>] [-l <length>] -a <address>
  echo_upd_client (-h | --help)
  echo_upd_client (-v | --version)

Options:
  -h --help         Show this screen.
  -v --version      Show version.
  -t <threads>      number of threads to use [default: 1].
  -l <length>       packet length in bytes [default: 100].
  -c <connections>  concurrent connections  [default: 100].
  -d <time>         time to run in seconds [default: 10].
  -a <address>      target address (e.g. 127.0.0.1:8080).
";

#[derive(Debug, Deserialize)]
struct Args {
    flag_c: usize,
    flag_l: usize,
    flag_t: usize,
    flag_d: usize,
    flag_v: bool,
    flag_a: String,
}

macro_rules! t {
    ($e:expr) => {
        match $e {
            Ok(val) => val,
            Err(err) => {
                println!("call = {:?}", stringify!($e));
                println!("err = {:?}", err);
                return;
            }
        }
    };
}

fn main() {
    let args: Args = Docopt::new(USAGE)
        .and_then(|d| d.deserialize())
        .unwrap_or_else(|e| e.exit());

    if args.flag_v {
        return println!("echo_udp_client: {}", VERSION);
    }

    let target_addr: &str = &args.flag_a;
    let test_msg_len = args.flag_l;
    let test_conn_num = args.flag_c;
    let test_seconds = args.flag_d;

    let err = io::Error::new(io::ErrorKind::Other, "can't resolve socket addresses");
    let addr = t!(target_addr.to_socket_addrs())
        .fold(Err(err), |prev, addr| prev.or(Ok(addr)))
        .unwrap();

    may::config().set_workers(args.flag_t);

    // let io_timeout = 5;
    let base_port = AtomicUsize::new(addr.port() as usize + 100);

    let stop = AtomicBool::new(false);
    let in_num = AtomicUsize::new(0);
    let out_num = AtomicUsize::new(0);

    let msg = vec![0; test_msg_len];

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
                let local_port = base_port.fetch_add(1, Ordering::Relaxed);
                let s = t!(UdpSocket::bind(("0.0.0.0", local_port as u16)));
                // t!(s.set_write_timeout(Some(Duration::from_secs(io_timeout))));
                // t!(s.set_read_timeout(Some(Duration::from_secs(io_timeout))));

                t!(s.connect(target_addr));

                let l = msg.len();
                let mut recv = vec![0; l];
                loop {
                    let mut rest = l;
                    while rest > 0 {
                        let i = t!(s.send(&msg[(l - rest)..l]));
                        rest -= i;
                    }

                    out_num.fetch_add(1, Ordering::Relaxed);

                    if stop.load(Ordering::Relaxed) {
                        break;
                    }

                    let mut rest = l;
                    while rest > 0 {
                        let i = t!(s.recv(&mut recv[(l - rest)..l]));
                        rest -= i;
                    }

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
