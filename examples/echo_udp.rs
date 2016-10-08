extern crate coroutine;
use coroutine::net::UdpSocket;
// use std::time::Duration;
// use std::io::ErrorKind;

/// simple test: echo hello | nc -u 127.0.0.1 30000

macro_rules! t {
    ($e: expr) => (match $e {
        Ok(val) => val,
        Err(err) => {
            println!("err = {:?}", err);
            return;
        }
    })
}

fn main() {
    coroutine::scheduler_set_workers(1);

    let h = coroutine::spawn(|| {
        let sock = UdpSocket::bind("127.0.0.1:30000").unwrap();
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

    h.join().unwrap();
}
