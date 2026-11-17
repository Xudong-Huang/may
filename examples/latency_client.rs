extern crate docopt;
#[macro_use]
extern crate may;
#[macro_use]
extern crate serde_derive;

use std::io::{Read, Write};
use std::net::ToSocketAddrs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use docopt::Docopt;
use may::coroutine;
use may::net::TcpStream;

const VERSION: &str = "0.1.0";

const USAGE: &str = "
Tcp echo latency client: persistent connections, RTT percentiles.

Usage:
  latency_client [-t <threads>] [-c <connections>] [-d <time>] [-l <length>] -a <address>
  latency_client (-h | --help)
  latency_client (-v | --version)

Options:
  -h --help         Show this screen.
  -v --version      Show version.
  -t <threads>      number of threads to use [default: 1].
  -l <length>       packet length in bytes [default: 100].
  -c <connections>  concurrent connections  [default: 100].
  -d <time>         time to run in seconds [default: 5].
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

fn percentile(samples: &[u64], p: usize) -> u64 {
    let idx = (samples.len() * p / 100).min(samples.len() - 1);
    samples[idx]
}

fn main() {
    let args: Args = Docopt::new(USAGE)
        .and_then(|d| d.deserialize())
        .unwrap_or_else(|e| e.exit());

    if args.flag_v {
        return println!("latency_client: {VERSION}");
    }

    may::config().set_workers(args.flag_t);

    let target_addr = args.flag_a.clone();
    let addr = target_addr
        .to_socket_addrs()
        .ok()
        .and_then(|mut addrs| addrs.next())
        .unwrap_or_else(|| panic!("can't resolve {target_addr}"));

    let msg_len = args.flag_l;
    let connections = args.flag_c;
    let seconds = args.flag_d;

    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();

    for _ in 0..connections {
        let tx = tx.clone();
        let stop = stop.clone();
        let msg = vec![0u8; msg_len];
        go!(move || {
            let mut samples = Vec::new();
            let mut conn = match TcpStream::connect(addr) {
                Ok(conn) => conn,
                Err(_) => {
                    tx.send(samples).unwrap();
                    return;
                }
            };
            let _ = conn.set_nodelay(true);
            let mut recv = vec![0u8; msg.len()];

            while !stop.load(Ordering::Relaxed) {
                let start = Instant::now();
                if conn.write_all(&msg).is_err() {
                    break;
                }
                if conn.read_exact(&mut recv).is_err() {
                    break;
                }
                samples.push(start.elapsed().as_nanos() as u64);
            }
            tx.send(samples).unwrap();
        });
    }

    drop(tx);
    go!(move || {
        coroutine::sleep(Duration::from_secs(seconds as u64));
        stop.store(true, Ordering::Release);
    });

    let mut all = Vec::new();
    for samples in rx {
        all.extend(samples);
    }
    if all.is_empty() {
        panic!("no latency samples collected");
    }
    all.sort_unstable();

    let count = all.len();
    let total: u128 = all.iter().map(|v| *v as u128).sum();
    let mean = total / count as u128;
    let p50 = percentile(&all, 50);
    let p95 = percentile(&all, 95);
    let p99 = percentile(&all, 99);
    let max = *all.last().unwrap();

    // Keep the benchmark script's generic parser working; append the latency
    // detail on the same line.
    println!(
        "Speed: {} request/sec, mean {} us, p50 {} us, p95 {} us, p99 {} us, max {} us",
        count as u64 / seconds as u64,
        mean / 1000,
        p50 / 1000,
        p95 / 1000,
        p99 / 1000,
        max / 1000
    );
    println!("{connections} connections, {msg_len} bytes, {seconds} sec, {count} samples");
}
