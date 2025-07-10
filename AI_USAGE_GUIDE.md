# May Rust Coroutine Library - AI Usage Guide

## Overview

**May** is a high-performance Rust library for stackful coroutines, providing Go-style goroutines for Rust. This guide provides comprehensive information for AI systems to properly understand, use, and contribute to the May codebase.

## 🎯 Core Concepts

### 1. Stackful Coroutines
- **Definition**: Each coroutine has its own stack (default 32KB on 64-bit systems)
- **Implementation**: Built on the `generator` library
- **Scheduling**: Cooperative scheduling across configurable worker threads
- **Memory**: Fixed stack size per coroutine (no automatic growth)

### 2. Go-style Concurrency
- **Philosophy**: Similar to Go's goroutines but with Rust safety guarantees
- **Spawning**: Use `go!` macro instead of direct `spawn` calls
- **Communication**: Channels (MPSC, MPMC, SPSC) and synchronization primitives

## 🚀 Getting Started

### Basic Coroutine Spawning

```rust
#[macro_use]
extern crate may;

// Simple coroutine
let handle = go!(|| {
    println!("Hello from coroutine!");
});
handle.join().unwrap();

// With custom stack size
let handle = go_with!(8192, || {
    println!("Coroutine with 8KB stack");
});

// Named coroutine with custom stack
let handle = go_with!("my_task", 16384, || {
    println!("Named coroutine with 16KB stack");
});
```

### Configuration

```rust
use may::config;

fn setup_runtime() {
    config()
        .set_workers(4)           // 4 worker threads
        .set_stack_size(0x2000)   // 8KB default stack
        .set_pool_capacity(1000)  // Coroutine pool size
        .set_worker_pin(true);    // Pin workers to CPU cores
}
```

## 📚 API Reference

### 1. Coroutine Management

#### Spawning Coroutines
```rust
// Preferred: Use go! macro (safe)
let handle = go!(|| {
    // coroutine code
});

// Advanced: Use Builder for custom configuration
let handle = go!(
    coroutine::Builder::new()
        .name("worker".to_string())
        .stack_size(0x4000),
    || {
        // coroutine code
    }
);

// Scoped coroutines (wait for all to complete)
coroutine::scope(|scope| {
    for i in 0..10 {
        go!(scope, move || {
            println!("Worker {}", i);
        });
    }
    // All coroutines complete before scope exits
});
```

#### Join Handles
```rust
let handle = go!(|| {
    42
});

// Wait for completion and get result
let result = handle.join().unwrap();
assert_eq!(result, 42);

// Check if done without blocking
if handle.is_done() {
    println!("Coroutine finished");
}

// Get coroutine handle for cancellation
let co = handle.coroutine();
unsafe { co.cancel(); } // Cancel the coroutine
```

### 2. Network I/O

#### TCP Server
```rust
use may::net::TcpListener;
use std::io::{Read, Write};

let listener = TcpListener::bind("127.0.0.1:8080")?;
for stream in listener.incoming() {
    let mut stream = stream?;
    go!(move || {
        let mut buf = [0; 1024];
        while let Ok(n) = stream.read(&mut buf) {
            if n == 0 { break; }
            stream.write_all(&buf[0..n])?;
        }
        Ok::<_, std::io::Error>(())
    });
}
```

#### UDP Socket
```rust
use may::net::UdpSocket;

let socket = UdpSocket::bind("127.0.0.1:8080")?;
let mut buf = [0; 1024];

loop {
    let (len, addr) = socket.recv_from(&mut buf)?;
    socket.send_to(&buf[0..len], addr)?;
}
```

#### Generic I/O Wrapper
```rust
use may::io::CoIo;
use std::fs::File;

// Wrap any I/O object for coroutine use
let file = File::open("example.txt")?;
let mut co_file = CoIo::new(file)?;

// Now can be used in coroutine context without blocking
let mut contents = String::new();
co_file.read_to_string(&mut contents)?;
```

### 3. Synchronization Primitives

#### Channels
```rust
use may::sync::mpsc;

// MPSC Channel
let (tx, rx) = mpsc::channel();
go!(move || {
    tx.send(42).unwrap();
});
let value = rx.recv().unwrap();

// MPMC Channel
use may::sync::mpmc;
let (tx, rx) = mpmc::channel();

// SPSC Channel (highest performance)
use may::sync::spsc;
let (tx, rx) = spsc::channel();
```

#### Mutex and RwLock
```rust
use may::sync::{Mutex, RwLock};
use std::sync::Arc;

// Mutex
let data = Arc::new(Mutex::new(0));
let data_clone = data.clone();

go!(move || {
    let mut guard = data_clone.lock().unwrap();
    *guard += 1;
});

// RwLock
let data = Arc::new(RwLock::new(vec![1, 2, 3]));
let reader = data.read().unwrap();
println!("Data: {:?}", *reader);
```

#### Semaphore and Barriers
```rust
use may::sync::{Semphore, Barrier};
use std::sync::Arc;

// Semaphore
let sem = Arc::new(Semphore::new(3)); // Allow 3 concurrent access
sem.wait(); // Acquire
sem.post(); // Release

// Barrier
let barrier = Arc::new(Barrier::new(5)); // Wait for 5 coroutines
let result = barrier.wait();
if result.is_leader() {
    println!("I'm the leader!");
}
```

### 4. Selection and Events

#### Select Operations
```rust
use may::sync::mpsc::channel;
use std::time::Duration;

let (tx1, rx1) = channel();
let (tx2, rx2) = channel();

// Select on multiple operations
let selected = select!(
    val = rx1.recv() => {
        println!("Received from rx1: {:?}", val);
        0
    },
    val = rx2.recv() => {
        println!("Received from rx2: {:?}", val);
        1
    },
    _ = may::coroutine::sleep(Duration::from_secs(1)) => {
        println!("Timeout occurred");
        2
    }
);
```

#### Custom Event Queues
```rust
use may::cqueue;

cqueue::scope(|cqueue| {
    // Add event sources
    go!(cqueue, 0, |es| {
        // Event source logic
        es.send(es.get_token());
    });
    
    // Poll for events
    match cqueue.poll(None) {
        Ok(event) => println!("Got event: {:?}", event),
        Err(e) => println!("Error: {:?}", e),
    }
});
```

### 5. Coroutine Local Storage

```rust
use may::coroutine_local;

coroutine_local! {
    static COUNTER: std::cell::RefCell<u32> = std::cell::RefCell::new(0);
}

go!(|| {
    COUNTER.with(|c| {
        *c.borrow_mut() += 1;
        println!("Counter: {}", *c.borrow());
    });
});
```

## ⚠️ Critical Safety Rules

### 1. No Thread-Blocking APIs
**NEVER** use thread-blocking operations in coroutines:

```rust
// ❌ BAD - Will block entire worker thread
std::thread::sleep(Duration::from_secs(1));
std::sync::Mutex::new(data).lock();
std::fs::File::open("file.txt");

// ✅ GOOD - Use May equivalents
may::coroutine::sleep(Duration::from_secs(1));
may::sync::Mutex::new(data).lock();
may::io::CoIo::new(std::fs::File::open("file.txt")?);
```

### 2. No Thread Local Storage (TLS)
```rust
use std::thread_local;

thread_local! {
    static TLS_VAR: u32 = 42; // ❌ Dangerous in coroutines
}

// ✅ Use Coroutine Local Storage instead
coroutine_local! {
    static CLS_VAR: u32 = 42;
}
```

### 3. Stack Size Management
```rust
// ❌ Avoid deep recursion
fn recursive_fn(n: u32) {
    if n > 0 {
        recursive_fn(n - 1); // Can overflow stack
    }
}

// ✅ Use iteration or increase stack size
let handle = go_with!(0x8000, || { // 32KB stack
    // More complex operations
});

// Debug stack usage (odd stack size)
let handle = go_with!(0x8000 - 1, || {
    // Stack usage will be printed on completion
});
```

### 4. CPU-Bound Tasks
```rust
// ❌ Long-running CPU tasks block scheduling
for i in 0..1_000_000 {
    heavy_computation();
}

// ✅ Yield periodically
for i in 0..1_000_000 {
    heavy_computation();
    if i % 1000 == 0 {
        may::coroutine::yield_now();
    }
}
```

## 🔧 Configuration and Tuning

### Runtime Configuration
```rust
use may::config;

fn configure_runtime() {
    config()
        .set_workers(num_cpus::get())    // Match CPU cores
        .set_stack_size(0x2000)          // 8KB stacks
        .set_pool_capacity(10000)        // Large pool for high concurrency
        .set_worker_pin(true)            // Pin to CPU cores
        .set_timeout_ns(10_000_000);     // 10ms I/O timeout
}
```

### Performance Tuning
```rust
// For high-concurrency servers
config()
    .set_workers(num_cpus::get() * 2)  // Oversubscribe for I/O bound
    .set_stack_size(0x1000)            // Smaller stacks for more coroutines
    .set_pool_capacity(50000);         // Large pool

// For CPU-intensive tasks
config()
    .set_workers(num_cpus::get())      // Match CPU cores exactly
    .set_stack_size(0x4000)            // Larger stacks for complex operations
    .set_pool_capacity(1000);          // Smaller pool
```

## 🐛 Error Handling and Debugging

### Panic Handling
```rust
let handle = go!(|| {
    panic!("Something went wrong!");
});

match handle.join() {
    Ok(result) => println!("Success: {:?}", result),
    Err(panic) => {
        if let Some(msg) = panic.downcast_ref::<&str>() {
            println!("Panic: {}", msg);
        }
    }
}
```

### Cancellation
```rust
let handle = go!(|| {
    loop {
        // Long-running task
        may::coroutine::sleep(Duration::from_millis(100));
        // Cancellation checks happen automatically at yield points
    }
});

// Cancel from another context
unsafe { handle.coroutine().cancel(); }

match handle.join() {
    Err(panic) => {
        if let Some(generator::Error::Cancel) = panic.downcast_ref() {
            println!("Coroutine was cancelled");
        }
    }
    _ => {}
}
```

### Stack Overflow Detection
```rust
// Stack overflow triggers segmentation fault
// Use guard pages and proper stack sizing
config().set_stack_size(0x4000); // Increase if needed

// Monitor stack usage in development
let handle = go_with!(0x2000 - 1, || { // Odd size enables monitoring
    // Complex operations
});
// Prints: "coroutine name = Some("name"), stack size = 8191, used size = 1234"
```

## 📁 Project Structure

### Key Modules
- **`src/coroutine.rs`** - Core coroutine API
- **`src/macros.rs`** - `go!` and other convenience macros
- **`src/net/`** - Async networking (TCP, UDP)
- **`src/sync/`** - Synchronization primitives
- **`src/io/`** - I/O abstractions and event loops
- **`src/scheduler.rs`** - Work-stealing scheduler
- **`src/config.rs`** - Runtime configuration

### Examples Directory
- **`examples/echo.rs`** - TCP echo server
- **`examples/http.rs`** - HTTP server
- **`examples/websocket.rs`** - WebSocket server
- **`examples/select.rs`** - Selection operations

## 🧪 Testing Patterns

### Unit Tests
```rust
#[test]
fn test_coroutine_spawn() {
    let handle = go!(|| {
        42
    });
    assert_eq!(handle.join().unwrap(), 42);
}

#[test]
fn test_channel_communication() {
    use may::sync::mpsc::channel;
    
    let (tx, rx) = channel();
    go!(move || {
        tx.send(42).unwrap();
    });
    
    assert_eq!(rx.recv().unwrap(), 42);
}
```

### Integration Tests
```rust
#[test]
fn test_tcp_echo_server() {
    use may::net::{TcpListener, TcpStream};
    use std::io::{Read, Write};
    
    // Start server
    go!(|| {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            go!(move || {
                let mut buf = [0; 1024];
                let n = stream.read(&mut buf).unwrap();
                stream.write_all(&buf[0..n]).unwrap();
            });
        }
    });
    
    // Test client
    may::coroutine::sleep(Duration::from_millis(10));
    let mut client = TcpStream::connect("127.0.0.1:8080").unwrap();
    client.write_all(b"hello").unwrap();
    
    let mut buf = [0; 5];
    client.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"hello");
}
```

## 🔍 Common Patterns

### Server Pattern
```rust
use may::net::TcpListener;

fn run_server() -> std::io::Result<()> {
    may::config().set_workers(4);
    
    let listener = TcpListener::bind("0.0.0.0:8080")?;
    println!("Server listening on {}", listener.local_addr()?);
    
    for stream in listener.incoming() {
        let stream = stream?;
        go!(move || {
            if let Err(e) = handle_client(stream) {
                eprintln!("Client error: {}", e);
            }
        });
    }
    Ok(())
}

fn handle_client(mut stream: TcpStream) -> std::io::Result<()> {
    let mut buf = vec![0; 1024];
    loop {
        let n = stream.read(&mut buf)?;
        if n == 0 { break; }
        stream.write_all(&buf[0..n])?;
    }
    Ok(())
}
```

### Worker Pool Pattern
```rust
use may::sync::mpsc::{channel, Receiver, Sender};

struct WorkerPool<T> {
    sender: Sender<T>,
}

impl<T: Send + 'static> WorkerPool<T> {
    fn new<F>(size: usize, handler: F) -> Self 
    where
        F: Fn(T) + Send + Sync + 'static + Clone,
    {
        let (sender, receiver) = channel();
        
        for _ in 0..size {
            let receiver = receiver.clone();
            let handler = handler.clone();
            go!(move || {
                while let Ok(item) = receiver.recv() {
                    handler(item);
                }
            });
        }
        
        WorkerPool { sender }
    }
    
    fn submit(&self, work: T) {
        self.sender.send(work).unwrap();
    }
}
```

### Producer-Consumer Pattern
```rust
use may::sync::mpsc::channel;

fn producer_consumer_example() {
    let (tx, rx) = channel();
    
    // Producer
    go!(move || {
        for i in 0..100 {
            tx.send(i).unwrap();
            may::coroutine::sleep(Duration::from_millis(10));
        }
    });
    
    // Consumer
    go!(move || {
        while let Ok(item) = rx.recv() {
            println!("Processing item: {}", item);
            // Process item
        }
    });
}
```

## 🚨 Common Pitfalls

### 1. Blocking Operations
```rust
// ❌ Will deadlock or block worker thread
let data = std::sync::Mutex::new(vec![1, 2, 3]);
let guard = data.lock().unwrap(); // Blocks thread

// ✅ Use May's mutex
let data = may::sync::Mutex::new(vec![1, 2, 3]);
let guard = data.lock().unwrap(); // Yields coroutine
```

### 2. Stack Overflow
```rust
// ❌ Deep recursion can overflow stack
fn fibonacci(n: u64) -> u64 {
    if n <= 1 { n } else { fibonacci(n-1) + fibonacci(n-2) }
}

// ✅ Use iteration or larger stack
let handle = go_with!(0x8000, || {
    fibonacci_iterative(100)
});
```

### 3. Resource Leaks
```rust
// ❌ Forgetting to join handles
for i in 0..1000 {
    go!(move || {
        println!("Task {}", i);
    }); // Handle dropped, coroutine may outlive parent
}

// ✅ Use scoped coroutines or join handles
coroutine::scope(|scope| {
    for i in 0..1000 {
        go!(scope, move || {
            println!("Task {}", i);
        });
    }
    // All coroutines complete before scope exits
});
```

## 📊 Performance Considerations

### Benchmarking
```rust
use std::time::Instant;

fn benchmark_coroutine_spawn() {
    let start = Instant::now();
    
    coroutine::scope(|scope| {
        for _ in 0..10000 {
            go!(scope, || {
                // Minimal work
            });
        }
    });
    
    println!("Spawned 10k coroutines in {:?}", start.elapsed());
}
```

### Memory Usage
```rust
// Monitor memory usage
config()
    .set_stack_size(0x1000)     // 4KB stacks
    .set_pool_capacity(10000);  // Pre-allocate pool

// For high-concurrency: smaller stacks, larger pool
// For complex tasks: larger stacks, smaller pool
```

### CPU Affinity
```rust
// Pin worker threads to CPU cores for better cache locality
config().set_worker_pin(true);

// Or disable if running in containers
config().set_worker_pin(false);
```

## 🔗 Integration with Other Libraries

### HTTP Servers
```rust
// Example integration pattern for HTTP libraries
use may::net::TcpListener;

fn http_server() -> std::io::Result<()> {
    let listener = TcpListener::bind("0.0.0.0:8080")?;
    
    for stream in listener.incoming() {
        let stream = stream?;
        go!(move || {
            // Parse HTTP request
            // Generate response
            // Write back to stream
        });
    }
    Ok(())
}
```

### Database Connections
```rust
// Wrap blocking database calls
use may::io::CoIo;

fn database_query() -> Result<Vec<Row>, Error> {
    // Use connection pooling with coroutine-safe primitives
    let conn = get_connection_from_pool()?;
    
    // For truly async database drivers, use them directly
    // For blocking drivers, consider using a worker thread pool
    Ok(conn.query("SELECT * FROM users")?)
}
```

## 📖 Additional Resources

### Documentation
- [Official Docs](https://docs.rs/may)
- [Repository](https://github.com/Xudong-Huang/may)
- [Examples](https://github.com/Xudong-Huang/may/tree/master/examples)

### Related Projects
- [generator-rs](https://github.com/Xudong-Huang/generator-rs) - Underlying generator library
- [may_minihttp](https://github.com/Xudong-Huang/may_minihttp) - HTTP server built on May

### Community
- Use GitHub issues for bug reports and feature requests
- Follow semantic versioning for API stability
- Check CHANGES.md for version-specific updates

---

## 🎉 Quick Start Checklist

1. **Add dependency**: `may = "0.3"`
2. **Add macro**: `#[macro_use] extern crate may;`
3. **Configure runtime**: `may::config().set_workers(4);`
4. **Spawn coroutines**: `go!(|| { /* code */ });`
5. **Use May I/O**: Replace std I/O with `may::net` and `may::io`
6. **Use May sync**: Replace std sync with `may::sync`
7. **Avoid blocking**: No thread-blocking operations
8. **Monitor stacks**: Use appropriate stack sizes

This guide provides the foundation for working effectively with the May coroutine library. Remember to always prioritize safety and follow the four critical rules to avoid undefined behavior. 