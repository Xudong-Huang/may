<div align="center">
    <h1>May</h1>
    <a href="https://travis-ci.org/Xudong-Huang/may">
        <img src="https://travis-ci.org/Xudong-Huang/may.svg?branch=master">
    </a>
    <a href="https://crates.io/crates/may">
        <img src="https://img.shields.io/crates/v/may.svg">
    </a>
    <a href="https://docs.rs/may">
        <img src="https://img.shields.io/badge/doc-may-green.svg">
    </a>
    
May is a high-performance library for programming stackful coroutines with which you can easily develop and maintain massive concurrent programs. It can be thought as the Rust version of the popular [Goroutine][go].
</div>

----------

## Table of contents
* [Features](#features)
* [Usage](#usage)
* [More examples](#more-examples)
  * [The CPU heavy load examples](#the-cpu-heavy-load-examples)
  * [The I/O heavy bound examples](#the-io-heavy-bound-examples)
* [Performance](#performance)
* [Caveat](#caveat)
* [How to tune a stack size](#how-to-tune-a-stack-size)
* [License](#license)

----------

## Features
* The stackful coroutine implementation is based on [generator][generator];
* Support schedule on a configurable number of threads for multi-core systems;
* Support coroutine version of a local storage ([CLS][cls]);
* Support efficient asynchronous network I/O;
* Support efficient timer management;
* Support standard synchronization primitives, a semaphore, an MPMC channel, etc;
* Support cancellation of coroutines;
* Support graceful panic handling that will not affect other coroutines;
* Support scoped coroutine creation;
* Support general selection for all the coroutine API;
* All the coroutine API are compatible with the standard library semantics;
* All the coroutine API can be safely called in multi-threaded context;
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
You can refer to https://tfb-status.techempower.com/ to get the latest [may_minihttp][may_minihttp] comparisons with other most popular frameworks.

----------

## Caveat
There is a detailed [document][caveat] that describes May's main restrictions. In general, there are four things you should follow when writing programs that use coroutines:
* Don't call thread-blocking API (It will hurt the performance);
* Carefully use Thread Local Storage (access TLS in coroutine might trigger undefined behavior).

> It's considered **unsafe** with the following pattern:
> ```rust
> set_tls();
> // Or another coroutine API that would cause scheduling:
> coroutine::yield_now(); 
> use_tls();
> ```
> but it's **safe** if your code is not sensitive about the previous state of TLS. Or there is no coroutines scheduling between **set** TLS and **use** TLS.

* Don't run CPU bound tasks for long time, but it's ok if you don't care about fairness;
* Don't exceed the coroutine stack. There is a guard page for each coroutine stack. When stack overflow occurs, it will trigger segment fault error.

**Note:**
> The first three rules are common when using cooperative asynchronous libraries in Rust. Even using a futures-based system also have these limitations. So what you should really focus on is a coroutine stack size, make sure it's big enough for your applications. 

----------

## How to tune a stack size
If you want to tune your coroutine stack size, please check out [this document][stack].

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
[tokio]:https://github.com/tokio-rs/tokio/blob/master/examples/echo.rs
[caveat]:docs/may_caveat.md
[stack]:docs/tune_stack_size.md
[may_minihttp]:https://github.com/Xudong-Huang/may_minihttp
