## Performance
Just a simple comparison with the Rust echo server implemented with [tokio][tokio] to get sense about May.

**Note:**
> The [Tokio-based][tokio] version is 0.2.18(2020-04-19). In theory, `future` scheduling is not evolving context switch which should be a little bit faster than the coroutine version. But I can't find a proper example for multi-threaded version comparison, so just put it here for you to get some sense about the performance of May. If you have a better implementation of s futures-based echo server, I will update it here. You can refer to https://tfb-status.techempower.com/ to get the latest [may_minihttp][may_minihttp] comparisons with other most popular frameworks.

**The machine's specification:**
  * **Logical Cores:** 8 (4 cores x 2 threads)
  * **Memory:** 4gb ECC DDR3 @ 1600mhz
  * **Processor:** CPU Intel(R) Core(TM) i7-3820QM CPU @ 2.70GHz
  * **Operating System:** Windows WSL2 Ubuntu 18.04

**An echo server and client:**

You can just compile it under this project:
```sh
$ cargo build --example=echo_client --release
```

**Tokio-based echo server:**

Run the server by default with 8 threads in another terminal:
```sh
$ cd tokio
$ cargo run --example=echo --release
```

```sh
$ cargo run --example=echo_client --release -- -c 100 -l 100 -t 8 -a 127.0.0.1:8080
    Finished release [optimized] target(s) in 5.86s
     Running `target/release/examples/echo_client -c 100 -l 100 -t 8 -a '127.0.0.1:8080'`
==================Benchmarking: 127.0.0.1:8080==================
100 clients, running 100 bytes, 10 sec.

Speed: 153496 request/sec,  153495 response/sec, 14989 kb/sec
Requests: 1534961
Responses: 1534958
```

**May-based echo server:**

Run the server by default with 2 threads in another terminal:
```sh
$ cd may
$ cargo run --example=echo --release -- -t 8 -p 8000
```

```sh
$ cargo run --example=echo_client --release -- -c 100 -l 100 -t 8 -a 127.0.0.1:8000
    Finished release [optimized] target(s) in 11.77s
     Running `target/release/examples/echo_client -c 100 -l 100 -t 8 -a '127.0.0.1:8000'`
==================Benchmarking: 127.0.0.1:8000==================
100 clients, running 100 bytes, 10 sec.

Speed: 256155 request/sec,  256155 response/sec, 25015 kb/sec
Requests: 2561559
Responses: 2561554
```

-----------
<!-- refs -->
[tokio]:https://github.com/tokio-rs/tokio/blob/master/examples/echo.rs
[may_minihttp]:https://github.com/Xudong-Huang/may_minihttp