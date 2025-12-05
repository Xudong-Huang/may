/// Integration tests based on examples directory
///
/// These tests mirror the functionality demonstrated in the examples/
/// directory but are designed for automated testing with proper setup,
/// teardown, and assertions. The original examples remain as user-facing
/// documentation and demonstrations.
#[macro_use]
extern crate may;

use may::coroutine::{spawn_safe, SafeBuilder, SafetyLevel};
use may::net::{TcpListener, TcpStream, UdpSocket};
use may::sync::mpsc;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// Helper function to find an available port for testing
fn find_available_port() -> u16 {
    use std::net::TcpListener as StdTcpListener;

    // Try to bind to port 0 to get an available port
    let listener = StdTcpListener::bind("127.0.0.1:0").expect("Failed to bind to available port");
    let port = listener
        .local_addr()
        .expect("Failed to get local address")
        .port();
    drop(listener);
    port
}

/// Helper function to wait for server to be ready
#[allow(dead_code)] // Available for future server tests
fn wait_for_server_ready(port: u16, timeout_ms: u64) -> bool {
    use std::net::TcpStream as StdTcpStream;

    let start = std::time::Instant::now();
    while start.elapsed().as_millis() < timeout_ms as u128 {
        if StdTcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

#[cfg(test)]
mod integration_tests {
    use super::*;

    /// Test suite for safe coroutine spawning functionality
    /// Based on examples/safe_spawn.rs
    mod safe_spawn_tests {
        use super::*;

        #[test]
        fn test_basic_safe_spawn() {
            may::config().set_workers(1);

            may::coroutine::scope(|_scope| {
                let counter = Arc::new(AtomicU32::new(0));
                let counter_clone = counter.clone();

                let handle = spawn_safe(move || {
                    for _i in 0..5 {
                        counter_clone.fetch_add(1, Ordering::SeqCst);
                        may::coroutine::yield_now();
                    }
                    "completed"
                })
                .expect("Failed to spawn safe coroutine");

                let result = handle.join().expect("Coroutine panicked");
                assert_eq!(result, "completed");
                assert_eq!(counter.load(Ordering::SeqCst), 5);
            });
        }

        #[test]
        fn test_safe_builder_configuration() {
            may::config().set_workers(1);

            may::coroutine::scope(|_scope| {
                let handle = SafeBuilder::new()
                    .name("test-coroutine".to_string())
                    .stack_size(64 * 1024)
                    .safety_level(SafetyLevel::Development)
                    .spawn(move || {
                        let current_coroutine = may::coroutine::current();
                        let name = current_coroutine.name();
                        assert_eq!(name, Some("test-coroutine"));
                        42
                    })
                    .expect("Failed to spawn configured coroutine");

                let result = handle.join().expect("Configured coroutine panicked");
                assert_eq!(result, 42);
            });
        }

        #[test]
        fn test_safe_coroutine_communication() {
            may::config().set_workers(1);

            may::coroutine::scope(|_scope| {
                let (tx, rx) = mpsc::channel();
                let message_count = 5;

                // Producer coroutine
                let tx_clone = tx.clone();
                drop(tx);
                let producer = spawn_safe(move || {
                    for i in 1..=message_count {
                        tx_clone
                            .send(format!("Message {i}"))
                            .expect("Send failed");
                        may::coroutine::yield_now();
                    }
                    drop(tx_clone);
                })
                .expect("Failed to spawn producer");

                // Consumer coroutine
                let consumer = spawn_safe(move || {
                    let mut messages = Vec::new();
                    while let Ok(msg) = rx.recv() {
                        messages.push(msg);
                        may::coroutine::yield_now();
                    }
                    messages
                })
                .expect("Failed to spawn consumer");

                // Wait for completion
                producer.join().expect("Producer panicked");
                let messages = consumer.join().expect("Consumer panicked");

                assert_eq!(messages.len(), message_count);
                for (i, msg) in messages.iter().enumerate() {
                    assert_eq!(msg, &format!("Message {}", i + 1));
                }
            });
        }

        #[test]
        fn test_different_safety_levels() {
            may::config().set_workers(1);

            may::coroutine::scope(|_scope| {
                // Test strict safety level
                let strict_handle = SafeBuilder::new()
                    .safety_level(SafetyLevel::Strict)
                    .spawn(|| "strict_result")
                    .expect("Failed to spawn strict coroutine");

                // Test permissive safety level
                let permissive_handle = SafeBuilder::new()
                    .safety_level(SafetyLevel::Permissive)
                    .spawn(|| "permissive_result")
                    .expect("Failed to spawn permissive coroutine");

                let strict_result = strict_handle.join().expect("Strict coroutine panicked");
                let permissive_result = permissive_handle
                    .join()
                    .expect("Permissive coroutine panicked");

                assert_eq!(strict_result, "strict_result");
                assert_eq!(permissive_result, "permissive_result");
            });
        }

        #[test]
        fn test_safe_spawn_error_handling() {
            // Test configuration validation
            let result = SafeBuilder::new()
                .stack_size(1024) // Too small - should fail
                .spawn(|| "should_not_run");

            assert!(result.is_err());
            let error_msg = result.unwrap_err().to_string();
            assert!(error_msg.contains("stack_size") || error_msg.contains("Stack size"));
        }
    }

    /// Test suite for scoped coroutine functionality
    /// Based on examples/scoped.rs
    mod scoped_tests {
        use super::*;

        #[test]
        fn test_scoped_coroutine_array_modification() {
            may::config().set_workers(1);

            let mut array = [1, 2, 3, 4, 5];
            let original_sum: i32 = array.iter().sum();

            may::coroutine::scope(|scope| {
                for i in &mut array {
                    go!(scope, move || {
                        may::coroutine::scope(|inner_scope| {
                            go!(inner_scope, || {
                                *i += 1;
                                may::coroutine::yield_now();
                            });
                        });
                        *i += 1;
                        may::coroutine::yield_now();
                    });
                }
            });

            let final_sum: i32 = array.iter().sum();
            // Each element should be incremented by 2 (once in inner scope, once in outer)
            assert_eq!(final_sum, original_sum + (array.len() as i32 * 2));
        }

        #[test]
        fn test_nested_scoped_coroutines() {
            may::config().set_workers(1);

            let counter = Arc::new(AtomicU32::new(0));
            let counter_clone = counter.clone();

            may::coroutine::scope(|outer_scope| {
                for _i in 0..3 {
                    let counter_ref = counter_clone.clone();
                    go!(outer_scope, move || {
                        may::coroutine::scope(|inner_scope| {
                            for _j in 0..2 {
                                let counter_inner = counter_ref.clone();
                                go!(inner_scope, move || {
                                    counter_inner.fetch_add(1, Ordering::SeqCst);
                                    may::coroutine::yield_now();
                                });
                            }
                        });
                    });
                }
            });

            // Should have 3 outer * 2 inner = 6 increments
            assert_eq!(counter.load(Ordering::SeqCst), 6);
        }
    }

    /// Test suite for TCP networking functionality
    /// Based on examples/echo.rs and examples/echo_client.rs
    mod tcp_networking_tests {
        use super::*;

        #[test]
        fn test_tcp_echo_server_client() {
            may::config().set_workers(2);

            let port = find_available_port();
            let server_ready = Arc::new(std::sync::Mutex::new(false));
            let server_ready_clone = server_ready.clone();

            may::coroutine::scope(|scope| {
                // Start echo server
                go!(scope, move || {
                    let listener = TcpListener::bind(("127.0.0.1", port))
                        .expect("Failed to bind TCP listener");

                    // Signal server is ready
                    {
                        let mut ready = server_ready_clone.lock().unwrap();
                        *ready = true;
                    }

                    // Handle one connection
                    if let Ok((mut stream, _)) = listener.accept() {
                        let mut buffer = vec![0; 1024];
                        loop {
                            match stream.read(&mut buffer) {
                                Ok(0) => break, // Connection closed
                                Ok(n) => {
                                    if stream.write_all(&buffer[0..n]).is_err() {
                                        break;
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                    }
                });

                // Wait for server to be ready
                while !*server_ready.lock().unwrap() {
                    may::coroutine::yield_now();
                }

                // Small delay to ensure server is listening
                may::coroutine::sleep(Duration::from_millis(10));

                // Start client
                go!(scope, move || {
                    let mut client = TcpStream::connect(("127.0.0.1", port))
                        .expect("Failed to connect to echo server");

                    let test_data = b"Hello, Echo Server!";
                    client
                        .write_all(test_data)
                        .expect("Failed to write to server");

                    let mut response = vec![0; test_data.len()];
                    client
                        .read_exact(&mut response)
                        .expect("Failed to read response");

                    assert_eq!(response, test_data);
                });
            });
        }

        #[test]
        fn test_tcp_multiple_clients() {
            may::config().set_workers(4);

            let port = find_available_port();
            let server_ready = Arc::new(std::sync::Mutex::new(false));
            let server_ready_clone = server_ready.clone();
            let client_count = 3;
            let responses_received = Arc::new(AtomicU32::new(0));

            may::coroutine::scope(|scope| {
                // Start echo server
                go!(scope, move || {
                    let listener = TcpListener::bind(("127.0.0.1", port))
                        .expect("Failed to bind TCP listener");

                    // Signal server is ready
                    {
                        let mut ready = server_ready_clone.lock().unwrap();
                        *ready = true;
                    }

                    // Handle multiple connections
                    for _ in 0..client_count {
                        if let Ok((mut stream, _)) = listener.accept() {
                            go!(move || {
                                let mut buffer = vec![0; 1024];
                                loop {
                                    match stream.read(&mut buffer) {
                                        Ok(0) => break,
                                        Ok(n) => {
                                            if stream.write_all(&buffer[0..n]).is_err() {
                                                break;
                                            }
                                        }
                                        Err(_) => break,
                                    }
                                }
                            });
                        }
                    }
                });

                // Wait for server to be ready
                while !*server_ready.lock().unwrap() {
                    may::coroutine::yield_now();
                }

                may::coroutine::sleep(Duration::from_millis(10));

                // Start multiple clients
                for i in 0..client_count {
                    let responses_clone = responses_received.clone();
                    go!(scope, move || {
                        let mut client = TcpStream::connect(("127.0.0.1", port))
                            .expect("Failed to connect to echo server");

                        let test_data = format!("Hello from client {i}");
                        client
                            .write_all(test_data.as_bytes())
                            .expect("Failed to write to server");

                        let mut response = vec![0; test_data.len()];
                        client
                            .read_exact(&mut response)
                            .expect("Failed to read response");

                        assert_eq!(response, test_data.as_bytes());
                        responses_clone.fetch_add(1, Ordering::SeqCst);
                    });
                }
            });

            assert_eq!(
                responses_received.load(Ordering::SeqCst),
                client_count as u32
            );
        }

        #[test]
        fn test_tcp_connection_handling() {
            may::config().set_workers(2);

            let port = find_available_port();
            let connections_handled = Arc::new(AtomicU32::new(0));
            let connections_clone = connections_handled.clone();

            may::coroutine::scope(|scope| {
                // Start server that counts connections
                go!(scope, move || {
                    let listener = TcpListener::bind(("127.0.0.1", port))
                        .expect("Failed to bind TCP listener");

                    // Handle 2 connections
                    for _ in 0..2 {
                        if let Ok((mut stream, _)) = listener.accept() {
                            connections_clone.fetch_add(1, Ordering::SeqCst);

                            // Simple echo once and close
                            let mut buffer = vec![0; 1024];
                            if let Ok(n) = stream.read(&mut buffer) {
                                if n > 0 {
                                    let _ = stream.write_all(&buffer[0..n]);
                                }
                            }
                        }
                    }
                });

                may::coroutine::sleep(Duration::from_millis(10));

                // Connect, send data, and disconnect
                go!(scope, move || {
                    let mut client1 = TcpStream::connect(("127.0.0.1", port))
                        .expect("Failed to connect client 1");
                    client1.write_all(b"test1").expect("Failed to write");

                    let mut response = vec![0; 5];
                    client1.read_exact(&mut response).expect("Failed to read");
                    assert_eq!(response, b"test1");
                });

                go!(scope, move || {
                    let mut client2 = TcpStream::connect(("127.0.0.1", port))
                        .expect("Failed to connect client 2");
                    client2.write_all(b"test2").expect("Failed to write");

                    let mut response = vec![0; 5];
                    client2.read_exact(&mut response).expect("Failed to read");
                    assert_eq!(response, b"test2");
                });
            });

            assert_eq!(connections_handled.load(Ordering::SeqCst), 2);
        }
    }

    /// Test suite for UDP networking functionality
    /// Based on examples/echo_udp.rs and examples/echo_udp_client.rs
    mod udp_networking_tests {
        use super::*;

        #[test]
        fn test_udp_echo_server_client() {
            may::config().set_workers(2);

            let port = find_available_port();
            let server_ready = Arc::new(std::sync::Mutex::new(false));
            let server_ready_clone = server_ready.clone();

            may::coroutine::scope(|scope| {
                // Start UDP echo server
                go!(scope, move || {
                    let socket =
                        UdpSocket::bind(("127.0.0.1", port)).expect("Failed to bind UDP socket");

                    // Signal server is ready
                    {
                        let mut ready = server_ready_clone.lock().unwrap();
                        *ready = true;
                    }

                    // Handle one echo request
                    let mut buffer = vec![0; 1024];
                    if let Ok((len, addr)) = socket.recv_from(&mut buffer) {
                        socket
                            .send_to(&buffer[0..len], addr)
                            .expect("Failed to echo UDP message");
                    }
                });

                // Wait for server to be ready
                while !*server_ready.lock().unwrap() {
                    may::coroutine::yield_now();
                }

                may::coroutine::sleep(Duration::from_millis(10));

                // Start UDP client
                go!(scope, move || {
                    let client =
                        UdpSocket::bind("127.0.0.1:0").expect("Failed to bind client socket");

                    let test_data = b"UDP Echo Test";
                    client
                        .send_to(test_data, ("127.0.0.1", port))
                        .expect("Failed to send UDP message");

                    let mut response = vec![0; test_data.len()];
                    let (len, _) = client
                        .recv_from(&mut response)
                        .expect("Failed to receive UDP response");

                    assert_eq!(len, test_data.len());
                    assert_eq!(&response[0..len], test_data);
                });
            });
        }

        #[test]
        fn test_udp_multiple_clients() {
            may::config().set_workers(4);

            let port = find_available_port();
            let server_ready = Arc::new(std::sync::Mutex::new(false));
            let server_ready_clone = server_ready.clone();
            let client_count = 3;
            let responses_received = Arc::new(AtomicU32::new(0));

            may::coroutine::scope(|scope| {
                // Start UDP echo server
                go!(scope, move || {
                    let socket =
                        UdpSocket::bind(("127.0.0.1", port)).expect("Failed to bind UDP socket");

                    // Signal server is ready
                    {
                        let mut ready = server_ready_clone.lock().unwrap();
                        *ready = true;
                    }

                    // Handle multiple echo requests
                    for _ in 0..client_count {
                        let mut buffer = vec![0; 1024];
                        if let Ok((len, addr)) = socket.recv_from(&mut buffer) {
                            socket
                                .send_to(&buffer[0..len], addr)
                                .expect("Failed to echo UDP message");
                        }
                    }
                });

                // Wait for server to be ready
                while !*server_ready.lock().unwrap() {
                    may::coroutine::yield_now();
                }

                may::coroutine::sleep(Duration::from_millis(10));

                // Start multiple UDP clients
                for i in 0..client_count {
                    let responses_clone = responses_received.clone();
                    go!(scope, move || {
                        let client =
                            UdpSocket::bind("127.0.0.1:0").expect("Failed to bind client socket");

                        let test_data = format!("UDP client {i}");
                        client
                            .send_to(test_data.as_bytes(), ("127.0.0.1", port))
                            .expect("Failed to send UDP message");

                        let mut response = vec![0; test_data.len()];
                        let (len, _) = client
                            .recv_from(&mut response)
                            .expect("Failed to receive UDP response");

                        assert_eq!(len, test_data.len());
                        assert_eq!(&response[0..len], test_data.as_bytes());
                        responses_clone.fetch_add(1, Ordering::SeqCst);
                    });
                }
            });

            assert_eq!(
                responses_received.load(Ordering::SeqCst),
                client_count as u32
            );
        }
    }

    /// Test suite for HTTP server functionality
    /// Based on examples/http.rs
    mod http_server_tests {
        use super::*;

        #[test]
        fn test_http_server_basic_requests() {
            may::config().set_workers(2);

            let port = find_available_port();
            let server_ready = Arc::new(std::sync::Mutex::new(false));
            let server_ready_clone = server_ready.clone();

            may::coroutine::scope(|scope| {
                // Start HTTP server
                go!(scope, move || {
                    let listener = TcpListener::bind(("127.0.0.1", port))
                        .expect("Failed to bind HTTP listener");

                    // Signal server is ready
                    {
                        let mut ready = server_ready_clone.lock().unwrap();
                        *ready = true;
                    }

                    // Handle 3 requests
                    for _ in 0..3 {
                        if let Ok((mut stream, _)) = listener.accept() {
                            go!(move || {
                                let mut buffer = Vec::new();
                                let mut temp_buf = vec![0; 1024];

                                // Read HTTP request
                                match stream.read(&mut temp_buf) {
                                    Ok(n) if n > 0 => {
                                        buffer.extend_from_slice(&temp_buf[0..n]);

                                        // Parse request to determine response
                                        let request = String::from_utf8_lossy(&buffer);
                                        let response = if request.contains("GET / ") {
                                            "HTTP/1.1 200 OK\r\nContent-Length: 27\r\n\r\nWelcome to May http demo\n"
                                        } else if request.contains("GET /hello ") {
                                            "HTTP/1.1 200 OK\r\nContent-Length: 14\r\n\r\nHello, World!\n"
                                        } else {
                                            "HTTP/1.1 404 Not Found\r\nContent-Length: 18\r\n\r\nCannot find page\n"
                                        };

                                        let _ = stream.write_all(response.as_bytes());
                                    }
                                    _ => {}
                                }
                            });
                        }
                    }
                });

                // Wait for server to be ready
                while !*server_ready.lock().unwrap() {
                    may::coroutine::yield_now();
                }

                may::coroutine::sleep(Duration::from_millis(10));

                // Test root path
                go!(scope, move || {
                    let mut client = TcpStream::connect(("127.0.0.1", port))
                        .expect("Failed to connect to HTTP server");

                    let request = "GET / HTTP/1.1\r\nHost: localhost\r\n\r\n";
                    client
                        .write_all(request.as_bytes())
                        .expect("Failed to send HTTP request");

                    let mut response = vec![0; 1024];
                    let n = client
                        .read(&mut response)
                        .expect("Failed to read HTTP response");
                    let response_str = String::from_utf8_lossy(&response[0..n]);

                    assert!(response_str.contains("200 OK"));
                    assert!(response_str.contains("Welcome to May http demo"));
                });

                // Test /hello path
                go!(scope, move || {
                    let mut client = TcpStream::connect(("127.0.0.1", port))
                        .expect("Failed to connect to HTTP server");

                    let request = "GET /hello HTTP/1.1\r\nHost: localhost\r\n\r\n";
                    client
                        .write_all(request.as_bytes())
                        .expect("Failed to send HTTP request");

                    let mut response = vec![0; 1024];
                    let n = client
                        .read(&mut response)
                        .expect("Failed to read HTTP response");
                    let response_str = String::from_utf8_lossy(&response[0..n]);

                    assert!(response_str.contains("200 OK"));
                    assert!(response_str.contains("Hello, World!"));
                });

                // Test 404 path
                go!(scope, move || {
                    let mut client = TcpStream::connect(("127.0.0.1", port))
                        .expect("Failed to connect to HTTP server");

                    let request = "GET /nonexistent HTTP/1.1\r\nHost: localhost\r\n\r\n";
                    client
                        .write_all(request.as_bytes())
                        .expect("Failed to send HTTP request");

                    let mut response = vec![0; 1024];
                    let n = client
                        .read(&mut response)
                        .expect("Failed to read HTTP response");
                    let response_str = String::from_utf8_lossy(&response[0..n]);

                    assert!(response_str.contains("404 Not Found"));
                    assert!(response_str.contains("Cannot find page"));
                });
            });
        }

        #[test]
        fn test_http_server_concurrent_requests() {
            may::config().set_workers(4);

            let port = find_available_port();
            let server_ready = Arc::new(std::sync::Mutex::new(false));
            let server_ready_clone = server_ready.clone();
            let request_count = 5;
            let responses_received = Arc::new(AtomicU32::new(0));

            may::coroutine::scope(|scope| {
                // Start HTTP server
                go!(scope, move || {
                    let listener = TcpListener::bind(("127.0.0.1", port))
                        .expect("Failed to bind HTTP listener");

                    // Signal server is ready
                    {
                        let mut ready = server_ready_clone.lock().unwrap();
                        *ready = true;
                    }

                    // Handle concurrent requests
                    for _ in 0..request_count {
                        if let Ok((mut stream, _)) = listener.accept() {
                            go!(move || {
                                let mut temp_buf = vec![0; 1024];

                                // Read and respond to HTTP request
                                if let Ok(n) = stream.read(&mut temp_buf) {
                                    if n > 0 {
                                        let response = "HTTP/1.1 200 OK\r\nContent-Length: 27\r\n\r\nWelcome to May http demo\n";
                                        let _ = stream.write_all(response.as_bytes());
                                    }
                                }
                            });
                        }
                    }
                });

                // Wait for server to be ready
                while !*server_ready.lock().unwrap() {
                    may::coroutine::yield_now();
                }

                may::coroutine::sleep(Duration::from_millis(10));

                // Send concurrent requests
                for i in 0..request_count {
                    let responses_clone = responses_received.clone();
                    go!(scope, move || {
                        let mut client = TcpStream::connect(("127.0.0.1", port))
                            .expect("Failed to connect to HTTP server");

                        let request =
                            format!("GET /?client={i} HTTP/1.1\r\nHost: localhost\r\n\r\n");
                        client
                            .write_all(request.as_bytes())
                            .expect("Failed to send HTTP request");

                        let mut response = vec![0; 1024];
                        let n = client
                            .read(&mut response)
                            .expect("Failed to read HTTP response");
                        let response_str = String::from_utf8_lossy(&response[0..n]);

                        if response_str.contains("200 OK") {
                            responses_clone.fetch_add(1, Ordering::SeqCst);
                        }
                    });
                }
            });

            assert_eq!(
                responses_received.load(Ordering::SeqCst),
                request_count as u32
            );
        }
    }

    /// Test suite for basic coroutine functionality
    /// Based on examples/sleep.rs, spawn.rs, etc.
    mod basic_coroutine_tests {
        use super::*;

        #[test]
        fn test_sleep_functionality() {
            may::config().set_workers(1);

            let start_time = std::time::Instant::now();

            may::coroutine::scope(|scope| {
                go!(scope, || {
                    may::coroutine::sleep(Duration::from_millis(50));
                });
            });

            let elapsed = start_time.elapsed();
            assert!(elapsed >= Duration::from_millis(45)); // Allow some tolerance
            assert!(elapsed < Duration::from_millis(200)); // But not too much
        }

        #[test]
        fn test_yield_now_functionality() {
            may::config().set_workers(1);

            let counter = Arc::new(AtomicU32::new(0));
            let counter_clone = counter.clone();

            may::coroutine::scope(|scope| {
                // Coroutine that yields frequently
                go!(scope, move || {
                    for _i in 0..10 {
                        counter_clone.fetch_add(1, Ordering::SeqCst);
                        may::coroutine::yield_now();
                    }
                });

                // Another coroutine that also increments
                let counter_clone2 = counter.clone();
                go!(scope, move || {
                    for _i in 0..10 {
                        counter_clone2.fetch_add(1, Ordering::SeqCst);
                        may::coroutine::yield_now();
                    }
                });
            });

            assert_eq!(counter.load(Ordering::SeqCst), 20);
        }

        #[test]
        fn test_coroutine_spawn_variations() {
            may::config().set_workers(2);

            may::coroutine::scope(|scope| {
                // Test basic spawn
                go!(scope, || {
                    assert_eq!(2 + 2, 4);
                });

                // Test spawn with move closure
                let value = 42;
                go!(scope, move || {
                    assert_eq!(value, 42);
                });

                // Test spawn with return value (using join)
                let handle = spawn_safe(|| "spawn_result").expect("Failed to spawn safe coroutine");

                let result = handle.join().expect("Spawn failed");
                assert_eq!(result, "spawn_result");
            });
        }
    }
}
