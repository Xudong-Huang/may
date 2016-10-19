extern crate coroutine;
// use std::time::Duration;
use std::io::{Read, Write};
use coroutine::net::{TcpListener, TcpStream};

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
    coroutine::scheduler_set_workers(1);

    coroutine::spawn(|| {
            // let listener = TcpListener::bind("127.0.0.1:8080").unwrap();
            let listener = TcpListener::bind("0.0.0.0:8080").unwrap();

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
