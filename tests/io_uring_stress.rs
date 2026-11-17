#![cfg(all(target_os = "linux", feature = "io_uring"))]

//! Single-worker stress scenarios for the io_uring backend.
//!
//! All io requests land on one shard here, which is what the timer-starvation
//! and connection-soak cases need. Keep this file to one test so the scheduler
//! is initialized once with one worker.

use std::io::{Read, Write};

#[macro_use]
extern crate may;

use may::net::TcpListener;

fn ring_supported() -> bool {
    match io_uring::IoUring::new(8) {
        Ok(_) => true,
        Err(e) => {
            eprintln!("skipping io_uring stress tests, ring unavailable: {e}");
            false
        }
    }
}

#[test]
fn timer_starvation_and_connection_soak() {
    if !ring_supported() {
        return;
    }

    // Must run before any may API initializes the scheduler.
    may::config().set_workers(1);

    #[cfg(feature = "io_timeout")]
    timer_starvation();
    connection_soak();
}

/// A short io timeout must fire even while the same (single) shard is parked
/// on a much later deadline from another request.
#[cfg(feature = "io_timeout")]
fn timer_starvation() {
    use std::io::ErrorKind;
    use std::sync::mpsc;
    use std::time::Duration;

    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();

    let peers = (0..2)
        .map(|_| {
            std::thread::spawn(move || {
                let _stream = std::net::TcpStream::connect(addr).unwrap();
                std::thread::sleep(Duration::from_millis(700));
            })
        })
        .collect::<Vec<_>>();

    let (mut long_stream, _) = listener.accept().unwrap();
    let (mut short_stream, _) = listener.accept().unwrap();
    long_stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    short_stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();

    let (tx_short, rx_short) = mpsc::channel();
    let short = go!(move || {
        let mut buf = [0u8; 1];
        let result = short_stream
            .read(&mut buf)
            .map(|_| ())
            .map_err(|e| e.kind());
        tx_short.send(result).unwrap();
    });

    let long = go!(move || {
        let mut buf = [0u8; 1];
        let result = long_stream.read(&mut buf).map(|_| ()).map_err(|e| e.kind());
        // Reaching here means the long timeout fired before it was canceled.
        let _ = result;
    });

    let result = rx_short
        .recv_timeout(Duration::from_secs(2))
        .expect("short io timeout was starved by the later timer");
    assert_eq!(result, Err(ErrorKind::TimedOut));

    short.join().unwrap();

    // The long read must still be parked; cancel it instead of waiting 30s.
    unsafe { long.coroutine().cancel() };
    assert!(long.join().is_err(), "long read was not canceled");

    for peer in peers {
        peer.join().unwrap();
    }
}

/// Many concurrent connections on one worker, each echoing a handful of
/// messages, to shake out CQ/SQ pressure and completion scheduling.
fn connection_soak() {
    const CONNECTIONS: usize = 100;
    const MESSAGES: usize = 10;
    const PAYLOAD: usize = 64;

    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();

    let peers = (0..CONNECTIONS)
        .map(|_| {
            std::thread::spawn(move || {
                let mut stream = std::net::TcpStream::connect(addr).unwrap();
                let mut sent = [0u8; PAYLOAD];
                sent[0] = 1;
                for _ in 0..MESSAGES {
                    stream.write_all(&sent).unwrap();
                    let mut echoed = [0u8; PAYLOAD];
                    stream.read_exact(&mut echoed).unwrap();
                    assert_eq!(echoed, sent);
                }
            })
        })
        .collect::<Vec<_>>();

    let mut handlers = Vec::with_capacity(CONNECTIONS);
    for _ in 0..CONNECTIONS {
        let (mut stream, _) = listener.accept().unwrap();
        handlers.push(go!(move || {
            let mut buf = [0u8; PAYLOAD];
            loop {
                let n = stream.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                stream.write_all(&buf[..n]).unwrap();
            }
        }));
    }

    for peer in peers {
        peer.join().unwrap();
    }
    for handler in handlers {
        handler.join().unwrap();
    }
}
