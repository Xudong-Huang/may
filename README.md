[![Build Status](https://travis-ci.org/Xudong-Huang/may.svg?branch=master)](https://travis-ci.org/Xudong-Huang/may)
[![Build status](https://ci.appveyor.com/api/projects/status/7gv4kw3b0m0y1iy6/branch/master?svg=true)](https://ci.appveyor.com/project/Xudong-Huang/may/branch/master)
[![Current Crates.io Version](https://img.shields.io/crates/v/may.svg)](https://crates.io/crates/may)
[![Document](https://img.shields.io/badge/doc-may-green.svg)](https://docs.rs/may)
[![License](https://img.shields.io/github/license/Xudong-Huang/may.svg)](https://github.com/Xudong-Huang/may/blob/master/LICENSE)


# MAY

Rust Stackfull Coroutine Library

## Features

* Stackfull coroutine implementation based on [generator][1]
* Support schedule on configurable number of threads for multi-cores
* Support coroutine version local storage([CLS][cls])
* Support efficient network async IO
* Support efficient timer management
* Support standard sync primitives plus semphore, mpmc channel and more
* Support cancellation of coroutines
* Support graceful panic handling that will not affect other coroutines
* Support scoped coroutine creation
* Support general select for all the coroutine APIs
* All the coroutine APIs are compatible with std library semantics
* All the coroutine APIs can be safely called in thread context


## Usage
```rust
#[macro_use]
extern crate generator;
use generator::Gn;

fn main() {
    let g = Gn::new_scoped(|mut s| {
        let (mut a, mut b) = (0, 1);
        while b < 200 {
            std::mem::swap(&mut a, &mut b);
            b = a + b;
            s.yield_(b);
        }
        done!();
    });

    for i in g {
        println!("{}", i);
    }
}
```

## More examples

### CPU heavy load examples
* [quick sort][2]
* [prime number generator][3]

### IO heavy Bound examples
* [echo server][4]
* [echo client][5]
* [coroutine based hyper][6]


## Performance
* As far as I can tell the io performance can compare with [mio][7], but I didn't test them throughly


## Notices
* both stable and nightly rust compiler are supported
* This crate supports below platforms, for more platform support, please ref [generator][1]

    - x86_64 Linux
    - x86_64 MacOs
    - x86_64 Windows

[1]:https://github.com/Xudong-Huang/generator-rs
[2]:https://github.com/Xudong-Huang/generator-rs
[3]:https://github.com/Xudong-Huang/generator-rs
[4]:https://github.com/Xudong-Huang/generator-rs
[5]:https://github.com/Xudong-Huang/generator-rs
[6]:https://github.com/Xudong-Huang/generator-rs
[7]:https://github.com/Xudong-Huang/generator-rs
[8]:https://github.com/Xudong-Huang/generator-rs
[9]:https://github.com/Xudong-Huang/generator-rs
[cls]:https://blog.zhpass.com/2017/12/18/CLS/
