extern crate coroutine;
// use std::time::Duration;
use std::io::{Read, Write};
// use std::os::unix::io::AsRawFd;
use coroutine::net::{TcpListener, TcpStream};

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
    // println!("got connection fd = {:?}", stream.as_raw_fd());
    // t!(stream.set_read_timeout(Some(Duration::from_secs(10))));
    // t!(stream.set_write_timeout(Some(Duration::from_secs(10))));
    let mut read = vec![0; 1024 * 16]; // alloc in heap!
    loop {
        match stream.read(&mut read) {
            Ok(n) => {
                if n == 0 {
                    // connection was closed
                    // println!("connection closed fd={}", stream.as_raw_fd());
                    break;
                }

                t!(stream.write_all(&read[0..n]));
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
