extern crate coroutine;
use coroutine::net::{TcpListener, TcpStream};
use std::io::{Read, Write};

fn handle_client(mut stream: TcpStream) {
    // read 20 bytes at a time from stream echoing back to stream
    loop {
        let mut read = [0; 1028];
        match stream.read(&mut read) {
            Ok(n) => {
                if n == 0 {
                    // connection was closed
                    break;
                }
                stream.write(&read[0..n]).unwrap();
            }
            Err(err) => {
                panic!(err);
            }
        }
    }
}


/// simple test: echo hello | nc 127.0.0.1 8080

fn main() {
    coroutine::scheduler_set_workers(4);
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
}
