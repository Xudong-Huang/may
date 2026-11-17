#![cfg(all(target_os = "linux", feature = "io_uring"))]

//! End-to-end tests for the completion-based io_uring backend.
//!
//! The backend is selected at compile time on Linux, so the event loop is
//! already io_uring here; the only runtime guard needed is for kernels or
//! containers where ring creation is not permitted.

#[macro_use]
extern crate may;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener as StdTcpListener};
use std::sync::mpsc;
use std::time::Duration;

use may::net::{TcpListener, TcpStream, UdpSocket};

fn ring_supported() -> bool {
    match io_uring::IoUring::new(8) {
        Ok(_) => true,
        Err(e) => {
            eprintln!("skipping io_uring tests, ring unavailable: {e}");
            false
        }
    }
}

fn run_in_coroutine<F, T>(f: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let handle = unsafe { may::coroutine::Builder::new().spawn(f) }.unwrap();
    handle.join().unwrap()
}

#[test]
fn tcp_accept_read_write() {
    if !ring_supported() {
        return;
    }

    run_in_coroutine(|| {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();

        let peer = std::thread::spawn(move || {
            let mut stream = std::net::TcpStream::connect(addr).unwrap();
            stream.write_all(b"hello").unwrap();
            let mut buf = [0u8; 5];
            stream.read_exact(&mut buf).unwrap();
            assert_eq!(&buf, b"world");
        });

        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 5];
        stream.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"hello");
        stream.write_all(b"world").unwrap();

        peer.join().unwrap();
    });
}

#[test]
fn tcp_peek_and_vectored_write() {
    if !ring_supported() {
        return;
    }

    run_in_coroutine(|| {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();

        let peer = std::thread::spawn(move || {
            let mut stream = std::net::TcpStream::connect(addr).unwrap();
            stream.write_all(b"hello").unwrap();
            let mut buf = [0u8; 4];
            stream.read_exact(&mut buf).unwrap();
            assert_eq!(&buf, b"abcd");
        });

        let (mut stream, _) = listener.accept().unwrap();

        // Peek must not consume the data.
        let mut peeked = [0u8; 2];
        assert_eq!(stream.peek(&mut peeked).unwrap(), 2);
        assert_eq!(&peeked, b"he");

        let mut buf = [0u8; 5];
        stream.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"hello");

        let bufs = [std::io::IoSlice::new(b"ab"), std::io::IoSlice::new(b"cd")];
        assert_eq!(stream.write_vectored(&bufs).unwrap(), 4);

        peer.join().unwrap();
    });
}

#[test]
fn udp_recv_from_and_send_to() {
    if !ring_supported() {
        return;
    }

    run_in_coroutine(|| {
        let server = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let server_addr = server.local_addr().unwrap();
        let client = std::net::UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let client_addr = client.local_addr().unwrap();

        client.send_to(b"ping", server_addr).unwrap();

        let mut buf = [0u8; 16];
        let (n, from) = server.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"ping");
        assert_eq!(from, client_addr);

        server.send_to(b"pong", from).unwrap();

        let mut reply = [0u8; 16];
        let (n, from) = client.recv_from(&mut reply).unwrap();
        assert_eq!(&reply[..n], b"pong");
        assert_eq!(from, server_addr);
    });
}

#[test]
fn plain_thread_io_uses_the_proxy_path() {
    if !ring_supported() {
        return;
    }

    let (tx, rx) = mpsc::channel();
    let server = go!(move || {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        tx.send(listener.local_addr().unwrap()).unwrap();
        let (mut stream, _) = listener.accept().unwrap();
        stream.write_all(b"pong").unwrap();
    });

    let addr: SocketAddr = rx.recv().unwrap();
    // This test thread is not a coroutine, so `connect`/`read` use the
    // thread-local proxy coroutine path.
    let mut stream = TcpStream::connect(addr).unwrap();
    #[cfg(feature = "io_timeout")]
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut buf = [0u8; 4];
    stream.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"pong");
    server.join().unwrap();
}

#[test]
fn udp_connected_send_recv() {
    if !ring_supported() {
        return;
    }

    run_in_coroutine(|| {
        use may::coroutine;

        let b = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let b_addr = b.local_addr().unwrap();
        let a = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let a_addr = a.local_addr().unwrap();
        a.connect(b_addr).unwrap();
        b.connect(a_addr).unwrap();

        coroutine::scope(|s| {
            go!(s, move || {
                coroutine::sleep(Duration::from_millis(50));
                a.send(b"ping").unwrap();
                let mut buf = [0u8; 16];
                let n = a.recv(&mut buf).unwrap();
                assert_eq!(&buf[..n], b"pong");
            });
            go!(s, move || {
                let mut buf = [0u8; 16];
                // The send above races this wait; if it is not ready yet the
                // completion backend submits a Recv request.
                let n = b.recv(&mut buf).unwrap();
                assert_eq!(&buf[..n], b"ping");
                b.send(b"pong").unwrap();
            });
        });
    });
}

#[test]
fn split_io_read_write() {
    if !ring_supported() {
        return;
    }

    run_in_coroutine(|| {
        use may::io::SplitIo;

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();

        let peer = std::thread::spawn(move || {
            let mut stream = std::net::TcpStream::connect(addr).unwrap();
            stream.write_all(b"ping").unwrap();
            let mut buf = [0u8; 4];
            stream.read_exact(&mut buf).unwrap();
            assert_eq!(&buf, b"pong");
        });

        let (stream, _) = listener.accept().unwrap();
        let (mut reader, mut writer) = stream.split().unwrap();
        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"ping");
        writer.write_all(b"pong").unwrap();

        peer.join().unwrap();
    });
}

#[test]
fn co_io_pipe_read_uses_the_ring() {
    if !ring_supported() {
        return;
    }

    run_in_coroutine(|| {
        use std::os::unix::io::FromRawFd;

        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);

        let reader = unsafe { std::fs::File::from_raw_fd(fds[0]) };
        let mut writer = unsafe { std::fs::File::from_raw_fd(fds[1]) };

        let peer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            writer.write_all(b"hi").unwrap();
        });

        // The pipe read end is non-blocking after `CoIo::new`; the first read
        // returns EAGAIN and the backend submits an IORING_OP_READ.
        let mut io = may::io::CoIo::new(reader).unwrap();
        let mut buf = [0u8; 2];
        io.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"hi");

        peer.join().unwrap();
    });
}

#[cfg(feature = "io_timeout")]
#[test]
fn sub_millisecond_read_timeout_fires() {
    if !ring_supported() {
        return;
    }

    run_in_coroutine(|| {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let peer = std::thread::spawn(move || {
            let _stream = std::net::TcpStream::connect(addr).unwrap();
            std::thread::sleep(Duration::from_millis(200));
        });

        let (mut stream, _) = listener.accept().unwrap();
        // std rejects an exactly-zero timeout, so use the smallest duration it
        // accepts; this still fires immediately in the backend.
        stream
            .set_read_timeout(Some(Duration::from_nanos(1)))
            .unwrap();
        let mut buf = [0u8; 1];
        let err = stream.read(&mut buf).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);

        peer.join().unwrap();
    });
}

#[cfg(feature = "io_timeout")]
#[test]
fn tcp_read_timeout() {
    if !ring_supported() {
        return;
    }

    run_in_coroutine(|| {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        // Hold the connection open without sending anything.
        let peer = std::thread::spawn(move || {
            let _stream = std::net::TcpStream::connect(addr).unwrap();
            std::thread::sleep(Duration::from_millis(600));
        });

        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(120)))
            .unwrap();
        let mut buf = [0u8; 1];
        let err = stream.read(&mut buf).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);

        peer.join().unwrap();
    });
}

#[test]
fn wait_io_waker_returns_the_forced_wake_bit() {
    if !ring_supported() {
        return;
    }

    const WAKE_FLAG: usize = 0x8000_0000;

    let (tx, rx) = mpsc::channel();
    let waiter = go!(move || {
        // A listening socket has no writability event, so the poll stays
        // pending until the waker cancels it.
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        use may::io::WaitIo;
        let waker = listener.waker();
        tx.send(waker).unwrap();
        listener.wait_io()
    });

    let waker = rx.recv().unwrap();
    waker.wakeup();
    let flags = waiter.join().unwrap();
    assert_eq!(flags & WAKE_FLAG, WAKE_FLAG);
}

#[test]
fn wait_io_waker_race_stress() {
    if !ring_supported() {
        return;
    }

    const WAKE_FLAG: usize = 0x8000_0000;

    for round in 0..200 {
        let (tx, rx) = mpsc::channel();
        let waiter = go!(move || {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            use may::io::WaitIo;
            let waker = listener.waker();
            tx.send(waker).unwrap();
            listener.wait_io()
        });

        let waker = rx.recv().unwrap();
        waker.wakeup();
        let flags = waiter.join().unwrap();
        assert_eq!(flags & WAKE_FLAG, WAKE_FLAG, "round {round}");
    }
}

#[test]
fn wait_io_reports_readiness() {
    if !ring_supported() {
        return;
    }

    run_in_coroutine(|| {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();

        let peer = std::thread::spawn(move || {
            let stream = std::net::TcpStream::connect(addr).unwrap();
            std::thread::sleep(Duration::from_millis(50));
            drop(stream);
        });

        // The connection makes the listener readable (POLLIN); a listening
        // socket is not writable, so the poll cannot complete spuriously.
        use may::io::WaitIo;
        let flags = listener.wait_io();
        assert_ne!(flags, 0, "wait_io returned no readiness bits");

        peer.join().unwrap();
    });
}

#[test]
fn connect_refused_reports_connection_refused() {
    if !ring_supported() {
        return;
    }

    // Bind and immediately close to get a port with no listener.
    let listener = StdTcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    run_in_coroutine(move || {
        let err = TcpStream::connect(addr).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::ConnectionRefused);
    });
}
