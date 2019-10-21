#[macro_use]
extern crate may;

use std::io::{Read, Write};

use may::coroutine;
use may::net::{TcpListener, TcpStream};

fn main() {
    // below config would schedule all the coroutines
    // on the single worker thread
    may::config().set_workers(1);

    // start the server
    let _server = go!(|| {
        let listener = TcpListener::bind("127.0.0.1:8000").unwrap();
        while let Ok((mut stream, _)) = listener.accept() {
            go!(move || {
                let mut buf = vec![0; 1024 * 8]; // alloc in heap!
                while let Ok(n) = stream.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    stream.write_all(&buf[0..n]).unwrap();
                }
            });
        }
    });

    // run some client until all finish
    coroutine::scope(|s| {
        for i in 0..100 {
            go!(s, move || {
                let mut buf = [i; 100];
                let mut conn = TcpStream::connect("127.0.0.1:8000").unwrap();
                conn.write_all(&buf).unwrap();
                conn.read_exact(&mut buf).unwrap();
                for v in buf.iter() {
                    assert_eq!(*v, i);
                }
            });
        }
    });
}
