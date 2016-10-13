extern crate coroutine;
use std::time::Duration;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use coroutine::net::TcpStream;

macro_rules! t {
    ($e: expr) => (match $e {
        Ok(val) => val,
        Err(err) => {
            println!("call = {:?}", stringify!($e));
            println!("err = {:?}", err);
            return;
        }
    })
}

fn main() {
    coroutine::scheduler_set_workers(1);

    // let target_addr = "127.0.0.1:5555";
    let target_addr = "127.0.0.1:8080";
    // let target_addr = "127.0.0.1:3000";
    let test_msg_len = 80;
    let test_conn_num = 100;
    let test_seconds = 20;
    let io_timeout = 2;

    let stop = AtomicBool::new(false);
    let in_num = AtomicUsize::new(0);
    let out_num = AtomicUsize::new(0);

    let msg = vec![0; test_msg_len];

    coroutine::scope(|scope| {
        scope.spawn(|| {
            coroutine::sleep(Duration::from_secs(test_seconds as u64));
            stop.store(true, Ordering::Release);
        });

        for _ in 0..test_conn_num {
            scope.spawn(|| {
                let mut conn = t!(TcpStream::connect(target_addr));
                // t!(conn.set_write_timeout(Some(Duration::from_secs(io_timeout))));
                // t!(conn.set_read_timeout(Some(Duration::from_secs(io_timeout))));
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

    println!("Benchmarking: {}", target_addr);
    println!("{} clients, running {} bytes, {} sec.\n",
             test_conn_num,
             test_msg_len,
             test_seconds);
    println!("Speed: {} request/sec,  {} response/sec",
             out_num / test_seconds,
             in_num / test_seconds);
    println!("Requests: {}", out_num);
    println!("Responses: {}", in_num);
}
