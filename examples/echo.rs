extern crate coroutine;
use coroutine::net::{TcpListener, TcpStream};
// use std::time::Duration;
use std::io::{Read, Write};

macro_rules! t {
    ($e: expr) => (match $e {
        Ok(val) => val,
        Err(err) => {
            println!("err = {:?}", err);
            return;
        }
    })
}

fn handle_client(mut stream: TcpStream) {
    // read 20 bytes at a time from stream echoing back to stream
    // t!(stream.set_read_timeout(Some(Duration::from_secs(10))));
    // t!(stream.set_write_timeout(Some(Duration::from_secs(10))));
    // let mut i = 0;
    let mut read = vec![0; 1024 * 16]; // alloc in heap!
    loop {
        match stream.read(&mut read) {
            Ok(n) => {
                if n == 0 {
                    // connection was closed
                    break;
                }

                let mut rest = n;
                while rest > 0 {
                    let i = t!(stream.write(&read[(n - rest)..n]));
                    rest -= i;
                }
            }

            Err(err) => {
                println!("err = {:?}", err);
                return;
            }
        }
    }
}


/// simple test: echo hello | nc 127.0.0.1 8080

fn main() {
    coroutine::scheduler_set_workers(1);

    coroutine::spawn(|| {
            let listener = TcpListener::bind("127.0.0.1:8080").unwrap();

            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        coroutine::spawn(move || {
                            handle_client(stream);
                        });
                    }
                    Err(_) => {
                        println!("Error");
                    }
                }
            }
        })
        .join()
        .unwrap();
}
