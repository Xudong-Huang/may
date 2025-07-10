# Advanced Coroutine Examples - Quick Reference

## Overview
This directory contains advanced coroutine examples demonstrating sophisticated concurrent programming patterns using the May library.

## Example Categories

### 🚀 Pipeline Processing
| Example | Purpose | Key Concepts |
|---------|---------|--------------|
| `pipeline_data_processing.rs` | Multi-stage data transformation | Pipelining, Backpressure, Flow Control |
| `stream_processing.rs` | Real-time stream analytics | Windowing, Aggregation, Hot Streams |

### 🌟 Fan-Out/Fan-In Patterns
| Example | Purpose | Key Concepts |
|---------|---------|--------------|
| `fan_out_fan_in.rs` | Work distribution & aggregation | Parallel Processing, Load Balancing |
| `scatter_gather.rs` | Distributed request processing | Concurrent Requests, Result Merging |

### 📊 Producer-Consumer Patterns
| Example | Purpose | Key Concepts |
|---------|---------|--------------|
| `producer_consumer_bounded.rs` | Bounded buffer with backpressure | Flow Control, Resource Management |
| `multi_stage_producer_consumer.rs` | Multi-stage processing | Pipeline Stages, Buffer Management |

### 💼 Real-World Applications
| Example | Purpose | Key Concepts |
|---------|---------|--------------|
| `web_crawler.rs` | Concurrent web crawling | Rate Limiting, URL Deduplication |
| `chat_server.rs` | Multi-room chat server | Pub/Sub, Connection Management |
| `file_processor.rs` | Batch file processing | File System Events, Batch Processing |

### 🔄 Advanced Synchronization
| Example | Purpose | Key Concepts |
|---------|---------|--------------|
| `worker_pool.rs` | Dynamic worker pool | Dynamic Scaling, Work Stealing |
| `circuit_breaker.rs` | Fault tolerance patterns | Circuit Breaker, Retry Logic |
| `rate_limiter.rs` | Advanced rate limiting | Token Bucket, Sliding Window |

### 🌐 Network & Protocol Patterns
| Example | Purpose | Key Concepts |
|---------|---------|--------------|
| `load_balancer.rs` | HTTP load balancer | Load Balancing, Health Checking |
| `proxy_server.rs` | HTTP/HTTPS forward proxy | Protocol Proxying, Streaming |
| `reverse_proxy.rs` | HTTP reverse proxy | Backend Pooling, SSL Termination |
| `pubsub_broker.rs` | Message broker | Topic Routing, Message Persistence |

## Quick Start Guide

### Prerequisites
```bash
# Ensure you have Rust installed
rustc --version

# Clone the repository
git clone https://github.com/microscaler/may.git
cd may/examples
```

### Running Examples

#### Basic Usage
```bash
# Run a pipeline processing example
cargo run --example pipeline_data_processing

# Run with custom configuration
cargo run --example web_crawler -- --max-concurrent 10 --delay 100ms
```

#### With Performance Monitoring
```bash
# Run with timing information
time cargo run --example fan_out_fan_in

# Run with memory profiling (requires valgrind)
valgrind --tool=massif cargo run --example worker_pool
```

## Pattern Cheat Sheet

### Pipeline Pattern
```rust
// Basic pipeline structure
may::coroutine::scope(|scope| {
    let (stage1_tx, stage1_rx) = mpsc::channel();
    let (stage2_tx, stage2_rx) = mpsc::channel();
    
    // Stage 1: Data Input
    go!(scope, move || {
        for data in input_source {
            stage1_tx.send(data).unwrap();
        }
    });
    
    // Stage 2: Processing
    go!(scope, move || {
        while let Ok(data) = stage1_rx.recv() {
            let processed = process(data);
            stage2_tx.send(processed).unwrap();
        }
    });
    
    // Stage 3: Output
    go!(scope, move || {
        while let Ok(data) = stage2_rx.recv() {
            output(data);
        }
    });
});
```

### Fan-Out/Fan-In Pattern
```rust
// Work distribution pattern
may::coroutine::scope(|scope| {
    let (work_tx, work_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    
    // Work generator
    go!(scope, move || {
        for work in work_items {
            work_tx.send(work).unwrap();
        }
    });
    
    // Multiple workers
    for _ in 0..num_workers {
        let work_rx = work_rx.clone();
        let result_tx = result_tx.clone();
        go!(scope, move || {
            while let Ok(work) = work_rx.recv() {
                let result = process_work(work);
                result_tx.send(result).unwrap();
            }
        });
    }
    
    // Result collector
    go!(scope, move || {
        while let Ok(result) = result_rx.recv() {
            collect_result(result);
        }
    });
});
```

### Producer-Consumer with Backpressure
```rust
// Bounded channel for backpressure
let (tx, rx) = mpsc::sync_channel(BUFFER_SIZE);

// Producer with backpressure handling
go!(scope, move || {
    for item in items {
        match tx.try_send(item) {
            Ok(_) => continue,
            Err(mpsc::TrySendError::Full(_)) => {
                // Handle backpressure
                may::coroutine::yield_now();
                tx.send(item).unwrap(); // Block until space available
            }
            Err(_) => break,
        }
    }
});
```

### Reverse Proxy Pattern
```rust
// Basic reverse proxy structure
may::coroutine::scope(|scope| {
    let backend_pool = Arc::new(BackendPool::new(vec![
        "http://backend1:8080".to_string(),
        "http://backend2:8080".to_string(),
    ]));
    
    let listener = TcpListener::bind("0.0.0.0:80").unwrap();
    
    for stream in listener.incoming() {
        let backend_pool = backend_pool.clone();
        go!(scope, move || {
            if let Ok(client_stream) = stream {
                // Select backend using load balancing algorithm
                let backend = backend_pool.select_backend();
                
                // Establish connection to backend
                if let Ok(backend_stream) = TcpStream::connect(&backend) {
                    // Proxy data bidirectionally
                    proxy_connection(client_stream, backend_stream);
                }
            }
        });
    }
});

// Key differences from forward proxy:
// - Forward Proxy: Client → Proxy → Internet (hides client)
// - Reverse Proxy: Internet → Proxy → Backends (hides servers)
```

## Performance Tips

### Optimization Guidelines
1. **Buffer Sizes** - Tune channel buffer sizes based on processing rates
2. **Worker Count** - Start with `num_cpus::get()` and adjust based on workload
3. **Yield Points** - Use `yield_now()` in CPU-intensive loops
4. **Resource Cleanup** - Always properly close channels and clean up resources

### Common Pitfalls
- **Deadlocks** - Ensure proper channel closure and avoid circular dependencies
- **Memory Leaks** - Close channels and drop unused handles
- **Starvation** - Use fair scheduling and avoid blocking operations
- **Resource Exhaustion** - Implement proper backpressure and limits

## Configuration Options

### Common Parameters
```rust
// Runtime configuration
may::config()
    .set_workers(num_cpus::get())           // Worker thread count
    .set_stack_size(2 * 1024 * 1024)       // Stack size per coroutine
    .set_pool_capacity(10000);              // Coroutine pool size

// Example-specific configuration
const BUFFER_SIZE: usize = 1000;            // Channel buffer size
const MAX_CONCURRENT: usize = 100;          // Max concurrent operations
const TIMEOUT_MS: u64 = 5000;               // Operation timeout
```

## Troubleshooting

### Common Issues
| Issue | Symptoms | Solution |
|-------|----------|----------|
| High Memory Usage | Growing memory consumption | Reduce buffer sizes, implement backpressure |
| Poor Performance | Slow execution, high CPU | Tune worker count, optimize hot paths |
| Deadlocks | Hanging execution | Check channel closure, avoid circular deps |
| Resource Leaks | Growing file descriptors | Ensure proper cleanup in error paths |

### Debug Commands
```bash
# Check for memory leaks
valgrind --leak-check=full cargo run --example <example_name>

# Profile performance
cargo build --release --example <example_name>
perf record ./target/release/examples/<example_name>
perf report

# Monitor resource usage
htop # or similar system monitor
```

## Testing Examples

### Unit Testing
```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_pipeline_processing() {
        // Test individual pipeline stages
        let input = vec![1, 2, 3, 4, 5];
        let output = run_pipeline(input);
        assert_eq!(output, expected_output);
    }
}
```

### Integration Testing
```bash
# Run all example tests
cargo test --examples

# Run specific example test
cargo test --example pipeline_data_processing
```

### Performance Testing
```bash
# Benchmark against sequential version
cargo bench --example fan_out_fan_in

# Load testing
cargo run --example chat_server &
# Use external load testing tool
```

## Contributing

### Adding New Examples
1. Follow the standard example structure
2. Include comprehensive documentation
3. Add unit and integration tests
4. Update this quick reference
5. Add entry to the main PRD

### Code Style
- Use `rustfmt` for formatting
- Follow clippy recommendations
- Include comprehensive error handling
- Add performance benchmarks where applicable

## Support

### Resources
- [May Documentation](https://docs.rs/may)
- [Rust Concurrency Book](https://rust-lang.github.io/async-book/)
- [Examples Repository](https://github.com/microscaler/may/tree/main/examples)

### Getting Help
- Open an issue on GitHub
- Check existing examples for patterns
- Review the comprehensive PRD document
- Join the Rust community discussions

---

*This quick reference is updated with each new example addition. Last updated: 2024-01-XX* 