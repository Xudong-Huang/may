/// Example demonstrating safe coroutine spawning with TLS safety checks
///
/// This example shows how to use the new safe spawn APIs that eliminate
/// the need for unsafe blocks while providing compile-time and runtime
/// safety guarantees.
use may::coroutine::{spawn_safe, SafeBuilder, SafetyLevel};
use may::sync::mpsc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

fn main() {
    println!("=== May Safe Coroutine Spawning Example ===\n");

    // Configure May runtime
    may::config().set_workers(1);

    // Run all examples within a coroutine scope to ensure proper execution
    may::coroutine::scope(|_scope| {
        // Example 1: Basic safe spawning
        println!("1. Basic safe coroutine spawning:");

        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();

        // This is safe - no unsafe block needed!
        let handle = spawn_safe(move || {
            for i in 0..5 {
                counter_clone.fetch_add(1, Ordering::SeqCst);
                println!("   Safe coroutine iteration: {i}");
                may::coroutine::yield_now();
            }
            "Safe coroutine completed"
        })
        .expect("Failed to spawn safe coroutine");

        // Wait for completion
        let result = handle.join().expect("Coroutine panicked");
        println!("   Result: {result}");
        println!(
            "   Final counter value: {}\n",
            counter.load(Ordering::SeqCst)
        );

        // Example 2: Using SafeBuilder for advanced configuration
        println!("2. Safe coroutine with custom configuration:");

        let handle = SafeBuilder::new()
            .name("configured-coroutine".to_string())
            .stack_size(64 * 1024) // 64KB stack
            .safety_level(SafetyLevel::Development) // Enhanced debugging
            .spawn(move || {
                println!("   Running in configured safe coroutine");
                println!("   Coroutine name: {:?}", may::coroutine::current().name());
                42
            })
            .expect("Failed to spawn configured coroutine");

        let result = handle.join().expect("Configured coroutine panicked");
        println!("   Configured coroutine result: {result}\n");

        // Example 3: Safe communication between coroutines
        println!("3. Safe coroutine communication:");

        let (tx, rx) = mpsc::channel();

        // Producer coroutine
        let tx_clone = tx.clone();
        drop(tx); // Drop the original sender so the channel closes properly when producer finishes
        let producer = spawn_safe(move || {
            for i in 1..=5 {
                tx_clone.send(format!("Message {i}")).expect("Send failed");
                println!("   Sent: Message {i}");
                may::coroutine::yield_now();
            }
            drop(tx_clone); // Close the channel
        })
        .expect("Failed to spawn producer");

        // Consumer coroutine
        let consumer = spawn_safe(move || {
            let mut messages = Vec::new();
            while let Ok(msg) = rx.recv() {
                println!("   Received: {msg}");
                messages.push(msg);
                may::coroutine::yield_now();
            }
            messages
        })
        .expect("Failed to spawn consumer");

        // Wait for both coroutines
        producer.join().expect("Producer panicked");
        let messages = consumer.join().expect("Consumer panicked");
        println!("   Total messages received: {}\n", messages.len());

        // Example 4: Different safety levels
        println!("4. Different safety levels:");

        // Strict safety level - maximum protection
        let strict_handle = SafeBuilder::new()
            .safety_level(SafetyLevel::Strict)
            .spawn(move || {
                println!("   Running with strict safety checks");
                "Strict mode"
            })
            .expect("Failed to spawn strict coroutine");

        // Permissive safety level - minimal overhead
        let permissive_handle = SafeBuilder::new()
            .safety_level(SafetyLevel::Permissive)
            .spawn(move || {
                println!("   Running with permissive safety checks");
                "Permissive mode"
            })
            .expect("Failed to spawn permissive coroutine");

        let strict_result = strict_handle.join().expect("Strict coroutine panicked");
        let permissive_result = permissive_handle
            .join()
            .expect("Permissive coroutine panicked");

        println!("   Strict result: {strict_result}");
        println!("   Permissive result: {permissive_result}\n");

        // Example 5: Error handling
        println!("5. Error handling with safe spawn:");

        // This demonstrates configuration validation
        match SafeBuilder::new()
            .stack_size(1024) // Too small - will fail validation
            .spawn(|| "This won't run")
        {
            Ok(_) => println!("   Unexpected success"),
            Err(e) => println!("   Expected error: {e}"),
        }

        println!("\n=== Safe Coroutine Example Complete ===");
        println!("All coroutines completed safely without unsafe blocks!");
    }); // End of coroutine scope
}

// Helper function to demonstrate TLS safety
fn _demonstrate_tls_safety() {
    // This would be detected as unsafe if we tried to access thread_local! storage
    // The safety system prevents such access at compile time or runtime

    println!(
        "This function demonstrates TLS safety - no thread-local access allowed in coroutines"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safe_spawn_basic() {
        let handle = spawn_safe(|| {
            println!("Test coroutine running");
            42
        })
        .expect("Failed to spawn test coroutine");

        let result = handle.join().expect("Test coroutine panicked");
        assert_eq!(result, 42);
    }

    #[test]
    fn test_safe_builder_validation() {
        // Valid configuration should work
        let result = SafeBuilder::new().stack_size(8192).spawn(|| "valid");
        assert!(result.is_ok());

        // Invalid configuration should fail
        let result = SafeBuilder::new()
            .stack_size(1024) // Too small
            .spawn(|| "invalid");
        assert!(result.is_err());
    }
}
