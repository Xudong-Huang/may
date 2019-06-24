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
    
May is a high-performant library for programming stackful coroutines with which you can easily develop and maintain massive concurrent programs. It can be thought as the Rust version of the popular [Goroutine][go].
</div>

----------

## Features
* The stackful coroutine's implementation is based on [generator][generator];
* Support schedule on a configurable number of threads for multi-core systems;
* Support coroutine's version of a local storage([CLS][cls]);
* Support efficient asynchronous network I/O;
* Support efficient timer management;
* Support standard synchronization primitives, a semaphore, an mpmc channel, etc;
* Support cancellation of coroutines;
* Support graceful panic handling that will not affect other coroutines;
* Support scoped coroutine creation;
* Support general selection for all the coroutine's API;
* All the coroutine's API are compatible with the standard library semantics;
* All the coroutine's API can be safely called in multithreaded context;
* Both stable, beta, and nightly channels are supported;
* Both x86_64 GNU/Linux, x86_64 Windows, x86_64 Mac OS are supported.
----------

## Usage
A naive echo server implemented with May:
```rust
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

----------

## More examples

### The CPU heavy load examples
* [The "Quick Sort" algorithm][sort]
* [A prime number generator][prime]

### The I/O heavy bound examples
* [An echo server][echo_server]
* [An echo client][echo_client]
* [A simple HTTP][http_sever]
* [A simple HTTPS][https_sever]
* [WebSockets][websocket]


----------

## Performance
Just a simple comparison with the Rust echo server implemented with [tokio][tokio] to get sense about May.

**Note:**
> The [Tokio-based][tokio] version is not at it's maximum optimization. In theory, `future` scheduling is not evolving context switch which should be a little bit faster than the coroutine version. But I can't find a proper example for multithreaded version comparison, so just put it here for you to get some sense about the performance of May. If you have a better implementation of s futures-based echo server, I will update it here.

**The machine's specification:**
  * **Logical Cores:** 4 (4 cores x 1 threads)
  * **Memory:** 4gb ECC DDR3 @ 1600mhz
  * **Processor:** CPU Intel(R) Core(TM) i7-3820QM CPU @ 2.70GHz
  * **Operating System:** Ubuntu VirtualBox guest

**An echo server and client:**

You can just compile it under this project:
```sh
$ cargo build --example=echo_client --release
```

**Tokio-based echo server:**

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

**May-based echo server:**

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

----------

## Caveat
There is a detailed [document][caveat] that describes May's main restrictions. In general, there are four things you should follow when writing programs that use coroutines:
* Don't call thread-blocking API (It will hurt the performance);
* Carefully use Thread Local Storage (access TLS in coroutine might trigger undefined behavior).

> It's considered **unsafe** with the following pattern:
> ```rust
> set_tls();
> // Or another coroutine's API that would cause scheduling:
> coroutine::yield_now(); 
> use_tls();
> ```
> but it's **safe** if your code is not sensitive about the previous state of TLS. Or there is no coroutines scheduling between **set** TLS and **use** TLS.

* Don't run CPU bound tasks for long time, but it's ok if you don't care about fairness;
* Don't exceed the coroutine stack. It will trigger undefined behavior.

**Note**
> The first three rules are common when using cooperative asynchronous libraries in Rust. Even using a futures-based system also have these limitations. So what you should really focus on is a coroutine's stack size, make sure it's big enough for your applications. 

----------

## How to tune a stack size
If you want to tune your coroutine's stack size, please check out [this document][stack].

----------

## License
May is licensed under either of the following, at your option:

 * The Apache License v2.0.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0);
 * The MIT License ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT).

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
