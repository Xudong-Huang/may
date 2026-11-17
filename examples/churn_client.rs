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
Tcp connection-churn client: connect, echo one message, close, repeat.

Usage:
  churn_client [-t <threads>] [-c <connections>] [-d <time>] [-l <length>] -a <address>
  churn_client (-h | --help)
  churn_client (-v | --version)

Options:
  -h --help         Show this screen.
  -v --version      Show version.
  -t <threads>      number of threads to use [default: 1].
  -l <length>       packet length in bytes [default: 100].
  -c <connections>  concurrent connection attempts [default: 100].
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

fn main() {
    let args: Args = Docopt::new(USAGE)
        .and_then(|d| d.deserialize())
        .unwrap_or_else(|e| e.exit());

    if args.flag_v {
        return println!("churn_client: {VERSION}");
    }

    may::config().set_workers(args.flag_t);

    let target_addr = args.flag_a.clone();
    let addr = target_addr
        .to_socket_addrs()
        .ok()
        .and_then(|mut addrs| addrs.next())
        .unwrap_or_else(|| panic!("can't resolve {target_addr}"));

    let test_msg_len = args.flag_l;
    let test_conn_num = args.flag_c;
    let test_seconds = args.flag_d;

    let stop = AtomicBool::new(false);
    let out_num = AtomicUsize::new(0);
    let in_num = AtomicUsize::new(0);
    let msg = vec![0; test_msg_len];

    coroutine::scope(|scope| {
        go!(scope, || {
            coroutine::sleep(Duration::from_secs(test_seconds as u64));
            stop.store(true, Ordering::Release);
        });

        go!(scope, || {
            let mut time = 0;
            let mut last_num = 0;
            while !stop.load(Ordering::Relaxed) {
                coroutine::sleep(Duration::from_secs(1));
                time += 1;
                let out = out_num.load(Ordering::Relaxed);
                let packets = out - last_num;
                last_num = out;
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
            go!(scope, || loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }

                let mut conn = match TcpStream::connect(addr) {
                    Ok(conn) => conn,
                    Err(_) => continue,
                };
                let _ = conn.set_nodelay(true);

                if conn.write_all(&msg).is_err() {
                    continue;
                }
                out_num.fetch_add(1, Ordering::Relaxed);

                let mut recv = vec![0; msg.len()];
                match conn.read_exact(&mut recv) {
                    Ok(()) => {
                        in_num.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                    Err(_) => continue,
                }
            });
        }
    });

    let out_num = out_num.load(Ordering::Relaxed);
    let in_num = in_num.load(Ordering::Relaxed);

    println!("==================Benchmarking: {target_addr}==================");
    println!("{test_conn_num} churn clients, running {test_msg_len} bytes, {test_seconds} sec.\n");
    println!(
        "Speed: {} request/sec,  {} response/sec, {} kb/sec",
        out_num / test_seconds,
        in_num / test_seconds,
        out_num * test_msg_len / test_seconds / 1024
    );
    println!("Requests: {out_num}");
    println!("Responses: {in_num}");
}
