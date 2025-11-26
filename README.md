<div align="center">
    <h1>May</h1>
    <a href="https://github.com/Xudong-Huang/may/actions?query=workflow%3ACI+branch%3Amaster">
        <img src="https://github.com/Xudong-Huang/may/workflows/CI/badge.svg">
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
* **Safe coroutine spawning** with compile-time and runtime safety guarantees (no unsafe blocks required);
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
* **Comprehensive safety infrastructure** with TLS safety and stack overflow protection;
* All the coroutine API are compatible with the standard library semantics;
* All the coroutine API can be safely called in multi-threaded context;
* Both stable, beta, and nightly channels are supported;
* x86_64 GNU/Linux, x86_64 Windows, x86_64 macOS, AArch64 GNU/Linux, and AArch64 macOS are supported.

----------

## Usage

### Safe Coroutine Spawning (Recommended)
The new safe API eliminates the need for unsafe blocks and provides comprehensive safety guarantees:

```rust
use may::coroutine::{spawn_safe, SafeBuilder, SafetyLevel};
use may::net::TcpListener;
use std::io::{Read, Write};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:8000")?;
    
    while let Ok((mut stream, _)) = listener.accept() {
        // Safe coroutine spawning - no unsafe blocks required!
        spawn_safe(move || -> Result<(), std::io::Error> {
            let mut buf = vec![0; 1024 * 16];
            while let Ok(n) = stream.read(&mut buf) {
                if n == 0 {
                    break;
                }
                stream.write_all(&buf[0..n])?;
            }
            Ok(())
        })?;
    }
    Ok(())
}
```

### Advanced Configuration
For fine-tuned control over safety and performance:

```rust
use may::coroutine::{SafeBuilder, SafetyLevel};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Configure coroutine with specific safety level
    let handle = SafeBuilder::new()
        .safety_level(SafetyLevel::Strict)
        .stack_size(1024 * 1024)
        .name("worker-coroutine")
        .spawn_safe(|| {
            println!("Safe coroutine with custom configuration!");
            42
        })?;
    
    let result = handle.join()?;
    println!("Result: {}", result);
    Ok(())
}
```

### Traditional API (Still Supported)
The traditional `go!` macro is still available for backward compatibility:

```rust
#[macro_use]
extern crate may;

use may::net::TcpListener;
use std::io::{Read, Write};

fn main() {
    let listener = TcpListener::bind("127.0.0.1:8000").unwrap();
    while let Ok((mut stream, _)) = listener.accept() {
        go!(move || {
            let mut buf = vec![0; 1024 * 16];
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

### Safety Examples
* [Safe coroutine spawning][safe_spawn] - Demonstrates the new safe APIs


----------

## Performance
You can refer to https://tfb-status.techempower.com/ to get the latest [may_minihttp][may_minihttp] comparisons with other most popular frameworks.

----------

## Safety Features

May now includes comprehensive safety infrastructure to help you write safer coroutine code:

### Safety Levels
- **Strict**: Maximum safety with runtime validation and TLS monitoring
- **Balanced**: Good safety with minimal performance overhead (recommended)
- **Permissive**: Basic safety for performance-critical code
- **Development**: Enhanced debugging and validation for development

### Automatic Safety Checks
```rust
use may::coroutine::{spawn_safe, SafetyLevel, SafeBuilder};

// The safe API automatically handles:
// - TLS access validation
// - Stack overflow detection
// - Blocking operation monitoring
// - Configuration validation

let handle = SafeBuilder::new()
    .safety_level(SafetyLevel::Strict)
    .spawn_safe(|| {
        // Your code here is automatically monitored for safety violations
        println!("Safe coroutine execution!");
        42
    })?;
```

### Safety Violation Handling
The safety system provides detailed error reporting:
```rust
use may::safety::SafetyViolation;

match spawn_safe(|| { /* your code */ }) {
    Ok(handle) => { /* success */ }
    Err(SafetyViolation::TlsAccess { description, .. }) => {
        eprintln!("TLS safety violation: {}", description);
    }
    Err(SafetyViolation::StackOverflow { current_usage, max_size, .. }) => {
        eprintln!("Stack overflow risk: {}/{} bytes", current_usage, max_size);
    }
    // ... other safety violations
}
```

## Caveat
There is a detailed [document][caveat] that describes May's main restrictions. With the new safe APIs, many of these concerns are automatically handled:

### Traditional Concerns (Automatically Handled by Safe APIs)
* ✅ **TLS Safety**: The safe API automatically detects and prevents unsafe TLS access patterns
* ✅ **Stack Overflow**: Runtime monitoring helps detect potential stack overflow conditions

### Still Important to Consider
* Don't call thread-blocking API (It will hurt the performance);
* Don't run CPU bound tasks for long time, but it's ok if you don't care about fairness;

**Note:**
> When using the new `spawn_safe` API with appropriate safety levels, most traditional coroutine safety concerns are automatically monitored and reported. For maximum safety, use `SafetyLevel::Strict` during development and testing. 

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
[safe_spawn]:examples/safe_spawn.rs
