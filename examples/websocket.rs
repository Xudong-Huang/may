#[macro_use]
extern crate may;
extern crate native_tls;
extern crate tungstenite;

use tungstenite::server::accept;
use may::net::TcpListener;

fn main() {
    let handler = go!(move || {
        let listener = TcpListener::bind(("0.0.0.0", 8080)).unwrap();
        for stream in listener.incoming() {
            go!(move || -> () {
                let mut websocket = accept(stream.unwrap()).unwrap();

                loop {
                    let msg = websocket.read_message().unwrap();

                    // Just echo back everything that the client sent to us
                    if msg.is_binary() || msg.is_text() {
                        websocket.write_message(msg).unwrap();
                    }
                }
            });
        }
    });

    println!("Websocket server running on ws://0.0.0.0:8080");
    handler.join().unwrap();
}
