<div align="center">
    <h1>May</h1>
    <a href="https://travis-ci.org/Xudong-Huang/may">
        <img src="https://travis-ci.org/Xudong-Huang/may.svg?branch=master">
    </a>
    <a href="https//ci.appveyor.com/project/Xudong-Huang/may/branch/master">
        <img src="https://ci.appveyor.com/api/projects/status/7gv4kw3b0m0y1iy6/branch/master?svg=true">
    </a>
    <a href="https://crates.io/crates/may">
        <img src="https://img.shields.io/crates/v/may.svg">
    </a>
    <a href="https://docs.rs/may">
        <img src="https://img.shields.io/badge/doc-may-green.svg">
    </a>
    
May is a high performance stackful coroutine library that can be thought of rust version [goroutine][go]. You can use it easily to design and develop massive concurrent programs in Rust.
</div>

## Features
* Stackful coroutine implementation based on [generator][generator];
* Support schedule on configurable number of threads for multi-cores;
* Support coroutine version local storage([CLS][cls]);
* Support efficient network async IO;
* Support efficient timer management;
* Support standard sync primitives plus semaphore, mpmc channel etc;
* Support cancellation of coroutine;
* Support graceful panic handling that will not affect other coroutines;
* Support scoped coroutine creation;
* Support general select for all the coroutine APIs;
* All the coroutine APIs are compatible with std library semantics;
* All the coroutine APIs can be safely called in thread context;


## Usage
```rust
/// a naive echo server
#[macro_use]
extern crate may;

use may::net::TcpListener;
use std::io::{Read, Write};

fn main() {
    let listener = TcpListener::bind("127.0.0.1:8000").unwrap();
    while let Ok((mut stream, _)) = listener.accept() {
        go!(move || {
            let mut buf = vec![0; 1024 * 16]; // alloc in heap!
            while let Ok(n) = stream.read(&mut buf) {
                if n == 0 {
                    break;
                }
                stream.write_all(&buf[0..n]).unwrap();
            }
        });
    }
}

```

## More examples

### CPU heavy load examples
* [Quick sort][sort]
* [A prime number generator][prime]

### IO heavy Bound examples
* [An echo server][echo_server]
* [An echo client][echo_client]
* [A simple http][http_sever]
* [A simple https][https_sever]
* [WebSockets][websocket]

## Performance
Just a simple comparison with the Rust echo server implemented in [tokio][tokio] to get a sense about May.

**Note**
> The [tokio][tokio] version is not at it's maximum optimization. In theory, `future` scheduling is not evolving context switch which should be a little bit faster than the coroutine version. But I can't find a proper example for multithreaded version comparison, so just put it here for you to get some sense about the performance of May. If you have a better implementation of `future` based echo server, I will update it here.

**Machine Specs:**
  * **Logical Cores:** 4 (4 cores x 1 threads)
  * **Memory:** 4gb ECC DDR3 @ 1600mhz
  * **Processor:** CPU Intel(R) Core(TM) i7-3820QM CPU @ 2.70GHz
  * **Operating System:** Ubuntu VirtualBox guest

**Echo server client:**
* You can just compile it under this project:
```sh
$ cargo build --example=echo_client --release
```

**tokio echo server**
Run the server by default with 2 threads in another terminal:
```sh
$ cd tokio-core
$ cargo run --example=echo-threads --release
```

```sh
$ target/release/examples/echo_client -t 2 -c 100 -l 100 -a 127.0.0.1:8080
==================Benchmarking: 127.0.0.1:8080==================
100 clients, running 100 bytes, 10 sec.

Speed: 315698 request/sec,  315698 response/sec, 30829 kb/sec
Requests: 3156989
Responses: 3156989
target/release/examples/echo_client -t 2 -c 100 -l 100 -a 127.0.0.1:8080  1.89s user 13.46s system 152% cpu 10.035 total
```

**may echo server**
Run the server by default with 2 threads in another terminal:
```sh
$ cd may
$ cargo run --example=echo --release -- -p 8000 -t 2
```

```sh
$ target/release/examples/echo_client -t 2 -c 100 -l 100 -a 127.0.0.1:8000
==================Benchmarking: 127.0.0.1:8000==================
100 clients, running 100 bytes, 10 sec.

Speed: 419094 request/sec,  419094 response/sec, 40927 kb/sec
Requests: 4190944
Responses: 4190944
target/release/examples/echo_client -t 2 -c 100 -l 100 -a 127.0.0.1:8000  2.60s user 16.96s system 195% cpu 10.029 total
```

## Caveat
There is a detailed [doc][caveat] that describes MAY's main restrictions.

There are four things you should avoid when writing coroutines:
* Don't call thread blocking APIs.
It will hurt the performance. 

* Carefully use Thread Local Storage.
Access TLS in coroutine may trigger undefined behavior.
> It's considered **unsafe** with the following pattern
> ```rust
> set_tls();
> coroutine::yield_now(); // or other coroutine api that would cause a scheduling
> use_tls();
> ```
> but it's **safe** if your code is not sensitive about the previous state of TLS. Or there is no coroutine scheduling between **set** TLS and **use** TLS.

* Don't run CPU bound tasks for long time, but it's ok if you don't care about fairness.
* Don't exceed the coroutine stack. It will trigger undefined behavior.

**Note**
> The first three rules are common when using cooperative async libraries in rust. Even using `future` based system also have these limitations. So what you should really focus on is the coroutine stack size, make sure it's big enough for your applications. 

## How to tune stack size
If you need to tune the coroutine stack size, please read [here][stack].

## Notices
* Both stable and nightly rust compiler are supported
* This crate supports below platforms, for more platform support, please ref [generator][generator]

    - x86_64 Linux
    - x86_64 MacOs
    - x86_64 Windows

## License
May is licensed under either of the following, at your option:

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT License ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

<!-- refs -->
[generator]:https://github.com/Xudong-Huang/generator-rs
[sort]:https://github.com/Xudong-Huang/quick_sort
[prime]:https://github.com/Xudong-Huang/prime
[echo_server]:examples/echo.rs
[echo_client]:examples/echo_client.rs
[http_sever]:examples/http.rs
[https_sever]:examples/https.rs
[websocket]:examples/websocket.rs
[cls]:docs/CLS_instead_of_TLS.md
[go]:https://tour.golang.org/concurrency/1
[tokio]:https://github.com/tokio-rs/tokio-core/blob/master/examples/echo-threads.rs
[caveat]:docs/may_caveat.md
[stack]:docs/tune_stack_size.md
