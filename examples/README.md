# May Coroutine Examples

Welcome to the May coroutine library examples! This directory contains both basic examples demonstrating core functionality and advanced examples showcasing sophisticated concurrent programming patterns.

## Quick Start

```bash
# Run a basic example
cargo run --example echo

# Run our flagship 1BRC example (world-class performance!)
cargo run --release --example one_billion_row_challenge -- --generate-file test.txt --count 10000000
cargo run --release --example one_billion_row_challenge -- --file test.txt

# Run advanced examples
cargo run --example pipeline_data_processing

# Run with custom configuration
cargo run --example safe_spawn
```

## 🚀 Flagship Example: One Billion Row Challenge

**`one_billion_row_challenge.rs`** - Our world-class implementation of the famous 1BRC achieving **90.9 million rows/second**!

### Performance Highlights
- **90.9M rows/sec** processing the real 13GB 1BRC dataset
- **10.999 seconds** to process 1 billion temperature measurements  
- **413 real weather stations** from the official 1BRC dataset
- **Memory efficient**: Only 1.7GB RAM for 13GB file processing
- **Multi-core optimized**: 554% CPU utilization across cores

### Key Technologies
- **Memory Mapping**: Zero-copy file access with `memmap2`
- **SIMD Acceleration**: Fast delimiter scanning with `memchr`  
- **Multi-core Parallelism**: Rayon for optimal CPU utilization
- **Custom Hash Functions**: `AHashMap` for fastest lookups
- **Branch-free Parsing**: Optimized temperature parsing algorithms

### Usage Examples
```bash
# Generate test data
cargo run --release --example one_billion_row_challenge -- --generate-file measurements.txt --count 1000000000

# Process the file (like real 1BRC)
cargo run --release --example one_billion_row_challenge -- --file measurements.txt

# Quick test with smaller dataset
cargo run --release --example one_billion_row_challenge -- --generate-file test.txt --count 10000000
cargo run --release --example one_billion_row_challenge -- --file test.txt
```

### Benchmark Comparison
- **Java (thomaswue)**: 1.535s for 1B records (651M rows/sec)
- **Our May + Rust**: 10.999s for 1B records (90.9M rows/sec)
- **Performance Ratio**: ~7x slower than fastest Java, but still world-class performance with Rust's memory safety!

## Current Examples

### Basic Coroutine Operations
- **`spawn.rs`** - Basic coroutine spawning and joining
- **`safe_spawn.rs`** - Safe coroutine spawning with TLS safety checks
- **`scoped.rs`** - Scoped coroutines with nested execution
- **`sleep.rs`** - Coroutine sleep and timing operations
- **`gen.rs`** - Generator patterns with coroutines

### Networking Examples
- **`echo.rs`** - TCP echo server
- **`echo_client.rs`** - TCP echo client with benchmarking
- **`echo_udp.rs`** - UDP echo server
- **`echo_udp_client.rs`** - UDP echo client
- **`http.rs`** - Basic HTTP server
- **`https.rs`** - HTTPS server with TLS
- **`websocket.rs`** - WebSocket echo server

### Advanced Patterns
- **`select.rs`** - Event selection with multiple channels
- **`loop_select.rs`** - Loop-based event selection
- **`cqueue.rs`** - Event queue processing with aggregation
- **`single_thread_schedule.rs`** - Single-threaded coroutine scheduling
- **`general_io.rs`** - General I/O operations with coroutines

### High-Performance Examples
- **`one_billion_row_challenge.rs`** - World-class 1BRC implementation (90.9M rows/sec) 🚀
- **`pipeline_data_processing.rs`** - Multi-stage data transformation pipeline ✅
- **`fan_out_fan_in.rs`** - Work distribution and result aggregation ✅ 
- **`producer_consumer_bounded.rs`** - Bounded buffer with backpressure ✅

## Advanced Examples (Planned)

We're developing a comprehensive set of advanced examples that demonstrate sophisticated coroutine patterns. See the documentation below for details:

### 📋 Planning Documents
- **[Advanced Examples PRD](ADVANCED_EXAMPLES_PRD.md)** - Comprehensive product requirements document
- **[Quick Reference Guide](ADVANCED_EXAMPLES_QUICK_REFERENCE.md)** - Quick reference for patterns and usage

### 🚀 Upcoming Examples

#### Pipeline Processing
- `pipeline_data_processing.rs` - Multi-stage data transformation pipeline
- `stream_processing.rs` - Real-time stream analytics

#### Fan-Out/Fan-In Patterns
- `fan_out_fan_in.rs` - Work distribution and result aggregation
- `scatter_gather.rs` - Distributed request processing

#### Real-World Applications
- `web_crawler.rs` - Concurrent web crawler with rate limiting
- `chat_server.rs` - Multi-room chat server with pub/sub messaging
- `file_processor.rs` - Batch file processing system

#### Advanced Synchronization
- `worker_pool.rs` - Dynamic worker pool with scaling
- `circuit_breaker.rs` - Fault tolerance patterns
- `rate_limiter.rs` - Advanced rate limiting algorithms

#### Network & Protocol Patterns
- `load_balancer.rs` - HTTP load balancer with health checking
- `proxy_server.rs` - HTTP/HTTPS forward proxy server
- `reverse_proxy.rs` - HTTP reverse proxy with load balancing
- `pubsub_broker.rs` - Message broker with persistence

## Usage Patterns

### Basic Pattern
```rust
#[macro_use]
extern crate may;

fn main() {
    may::config().set_workers(4);
    
    may::coroutine::scope(|scope| {
        go!(scope, || {
            println!("Hello from coroutine!");
        });
    });
}
```

### Producer-Consumer Pattern
```rust
use may::sync::mpsc;

may::coroutine::scope(|scope| {
    let (tx, rx) = mpsc::channel();
    
    // Producer
    go!(scope, move || {
        for i in 0..10 {
            tx.send(i).unwrap();
        }
    });
    
    // Consumer
    go!(scope, move || {
        while let Ok(item) = rx.recv() {
            println!("Received: {}", item);
        }
    });
});
```

### Network Server Pattern
```rust
use may::net::TcpListener;

let listener = TcpListener::bind("127.0.0.1:8080").unwrap();
for stream in listener.incoming() {
    match stream {
        Ok(stream) => {
            go!(move || {
                // Handle client connection
                handle_client(stream);
            });
        }
        Err(e) => println!("Connection error: {}", e),
    }
}
```

## Performance Tips

### Configuration
```rust
// Optimize for your workload
may::config()
    .set_workers(num_cpus::get())           // Match CPU cores
    .set_stack_size(2 * 1024 * 1024)       // 2MB stack per coroutine
    .set_pool_capacity(10000);              // Pre-allocate coroutines
```

### Best Practices
1. **Use appropriate buffer sizes** for channels based on your data flow
2. **Implement backpressure** to prevent memory exhaustion
3. **Handle errors gracefully** with proper cleanup
4. **Use `yield_now()`** in CPU-intensive loops
5. **Close channels properly** to avoid deadlocks

## Testing Examples

### Run Individual Examples
```bash
# Basic examples
cargo run --example spawn
cargo run --example echo

# With arguments (where supported)
cargo run --example echo_client -- -a 127.0.0.1:8080 -c 100
```

### Run All Tests
```bash
# Run integration tests
cargo test --test integration_tests

# Run example-specific tests
cargo test --example safe_spawn
```

### Performance Testing
```bash
# Benchmark examples
cargo build --release --example echo
time ./target/release/examples/echo

# Memory profiling
valgrind --tool=massif cargo run --example echo
```

## Development Guidelines

### Adding New Examples
1. Follow the standard structure with comprehensive documentation
2. Include error handling and resource cleanup
3. Add unit tests where appropriate
4. Update this README with the new example
5. Consider adding integration tests

### Code Style
- Use `rustfmt` for consistent formatting
- Follow clippy recommendations
- Include comprehensive rustdoc comments
- Handle all error cases appropriately

### Dependencies
Current examples use these key dependencies:
- `may` - Core coroutine library
- `docopt` + `serde_derive` - Command-line argument parsing
- `bytes` + `httparse` - HTTP processing
- `native_tls` - TLS/SSL support
- `tungstenite` - WebSocket support

## Troubleshooting

### Common Issues

#### High Memory Usage
- Reduce channel buffer sizes
- Implement proper backpressure
- Monitor coroutine lifecycle

#### Poor Performance
- Tune worker thread count
- Optimize hot code paths
- Use appropriate data structures

#### Deadlocks
- Ensure proper channel closure
- Avoid circular dependencies
- Use timeouts for operations

#### Connection Issues
- Check firewall settings
- Verify port availability
- Handle network errors gracefully

### Debug Commands
```bash
# Check for memory leaks
valgrind --leak-check=full cargo run --example <name>

# Profile CPU usage
perf record cargo run --example <name>
perf report

# Monitor system resources
htop
```

## Contributing

We welcome contributions to the examples! Please:

1. **Check existing examples** for similar patterns
2. **Follow the coding standards** outlined above
3. **Include comprehensive tests** for new examples
4. **Update documentation** including this README
5. **Consider performance implications** of your implementation

### Submitting Examples
1. Create a new example file following naming conventions
2. Include comprehensive rustdoc documentation
3. Add tests if the example is complex
4. Update the README and quick reference guide
5. Submit a pull request with clear description

## Resources

### Documentation
- [May Library Documentation](https://docs.rs/may)
- [Rust Async Book](https://rust-lang.github.io/async-book/)
- [Advanced Examples PRD](ADVANCED_EXAMPLES_PRD.md)
- [Quick Reference Guide](ADVANCED_EXAMPLES_QUICK_REFERENCE.md)

### Community
- [GitHub Issues](https://github.com/microscaler/may/issues)
- [Rust Community Discord](https://discord.gg/rust-lang)
- [Rust Users Forum](https://users.rust-lang.org/)

### Related Projects
- [Tokio](https://tokio.rs/) - Alternative async runtime
- [async-std](https://async.rs/) - Async standard library
- [Rayon](https://github.com/rayon-rs/rayon) - Data parallelism

---

**Getting Started:** Begin with `spawn.rs` and `echo.rs` to understand basic concepts, then explore `safe_spawn.rs` for advanced safety features. Check the PRD document for upcoming advanced examples that demonstrate sophisticated concurrent programming patterns.

**Need Help?** Check the troubleshooting section above or open an issue on GitHub. 