extern crate may;
extern crate httparse;
extern crate bytes;

use may::coroutine;
use may::net::TcpListener;
use httparse::Request;
use httparse::Status;
use bytes::BufMut;
use std::io::{Read, Write};

fn req_done(buf: &[u8], path: &mut String) -> Option<usize> {
    let mut headers = [httparse::EMPTY_HEADER; 16];
    let mut req = httparse::Request::new(&mut headers);

    if let Ok(Status::Complete(i)) = req.parse(buf) {
        path.clear();
        path.push_str(req.path.unwrap_or("/"));
        return Some(i);
    }

    None
}

fn main() {
    may::config().set_io_workers(4);

    let listener = TcpListener::bind("127.0.0.1:8080").unwrap();
    while let Ok((mut stream, _)) = listener.accept() {
        coroutine::spawn(move || {
            let mut buf = Vec::new();
            let mut path = String::new();

            loop {
                if let Some(i) = req_done(&buf, &mut path) {
                    let response = match &*path {
                        "/" => "Welcome to May http demo\n",
                        "/hello" => "Hello, World!\n",
                        "/quit" => std::process::exit(1),
                        _ => "Cannot find page\n"
                    };
                   
                    write!(stream, "\
                    HTTP/1.1 200 OK\r\n\
                    Server: May\r\n\
                    Content-Length: {}\r\n\
                    Date: 1-1-2000\r\n\
                    \r\n\
                    {}", response.len(), response);
                    
                    buf = buf.split_off(i);
                } else {
                    let mut tempBuf = vec![0; 512];
                    match stream.read(&mut tempBuf) {
                        Ok(0) => return, // connection was closed
                        Ok(n) => {
                            buf.put(&tempBuf[0..n]);
                        },
                        Err(err) => println!("err = {:?}", err),
                    }
                }
            }
        });
    }
}
