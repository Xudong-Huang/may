extern crate bytes;
extern crate httparse;
#[macro_use]
extern crate may;
extern crate native_tls;

use bytes::BufMut;
use httparse::Status;
use may::net::TcpListener;
use native_tls::{Identity, TlsAcceptor};
use std::fs::File;
use std::io::{Read, Write};
use std::sync::Arc;

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
    may::config().set_workers(4);

    let mut file = File::open("examples/cert/mycert.pfx").unwrap();
    let mut identity = vec![];
    file.read_to_end(&mut identity).unwrap();
    let identity = Identity::from_pkcs12(&identity, "password").unwrap();
    let acceptor = TlsAcceptor::new(identity).unwrap();
    let acceptor = Arc::new(acceptor);

    let listener = TcpListener::bind("127.0.0.1:8080").unwrap();
    while let Ok((stream, _)) = listener.accept() {
        let acceptor = acceptor.clone();
        let mut stream = acceptor.accept(stream).unwrap();

        go!(move || {
            let mut buf = Vec::new();
            let mut path = String::new();

            loop {
                if let Some(i) = req_done(&buf, &mut path) {
                    let response = match &*path {
                        "/" => "Welcome to May http demo\n",
                        "/hello" => "Hello, World!\n",
                        "/quit" => std::process::exit(1),
                        _ => "Cannot find page\n",
                    };

                    let s = format!(
                        "\
                         HTTP/1.1 200 OK\r\n\
                         Server: May\r\n\
                         Content-Length: {}\r\n\
                         Date: 1-1-2000\r\n\
                         \r\n\
                         {}",
                        response.len(),
                        response
                    );

                    stream
                        .write_all(s.as_bytes())
                        .expect("Cannot write to socket");

                    buf = buf.split_off(i);
                } else {
                    let mut temp_buf = vec![0; 512];
                    match stream.read(&mut temp_buf) {
                        Ok(0) => return, // connection was closed
                        Ok(n) => buf.put(&temp_buf[0..n]),
                        Err(err) => println!("err = {err:?}"),
                    }
                }
            }
        });
    }
}
