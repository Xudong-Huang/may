/// # Producer-Consumer Example
/// 
/// ## Description
/// This example demonstrates the producer-consumer pattern using May coroutines.
/// Multiple producers generate data at different rates while multiple consumers process it,
/// with proper coordination and comprehensive metrics.
/// 
/// ## Architecture
/// ```text
/// [Producer 1] ┐
/// [Producer 2] ├─→ [Channel] ─→ [Consumer 1] ┐
/// [Producer 3] ┘                [Consumer 2] ├─→ [Results]
///                                [Consumer 3] ┘
/// ```
/// 
/// ## Key Features
/// - Multiple producers with different production rates
/// - Multiple consumers with different processing speeds
/// - Comprehensive metrics and monitoring
/// - Graceful shutdown coordination
/// - Error handling and recovery
/// 
/// ## Use Cases
/// - Stream processing systems
/// - Load balancing between producers and consumers
/// - Multi-stage data processing pipelines
/// - Event-driven architectures
/// 
/// ## Usage
/// ```bash
/// cargo run --example producer_consumer_bounded
/// cargo run --example producer_consumer_bounded -- --producers 3 --consumers 2
/// ```

#[macro_use]
extern crate may;

use may::sync::mpsc;
use may::coroutine;
use std::sync::atomic::{AtomicU64, AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::collections::HashMap;
use rand;

/// Configuration for the producer-consumer example
#[derive(Debug, Clone, Copy)]
struct ProducerConsumerConfig {
    num_producers: usize,
    num_consumers: usize,
    total_items: usize,
    producer_rates: [u64; 4], // Items per second for each producer (up to 4)
    consumer_rates: [u64; 4], // Items per second for each consumer (up to 4)
    enable_backpressure_logging: bool,
    shutdown_timeout_secs: u64,
}

impl Default for ProducerConsumerConfig {
    fn default() -> Self {
        Self {
            num_producers: 2,
            num_consumers: 2,
            total_items: 1000,
            producer_rates: [100, 150, 200, 250], // Different production rates
            consumer_rates: [80, 120, 160, 200],  // Different consumption rates
            enable_backpressure_logging: true,
            shutdown_timeout_secs: 30,
        }
    }
}

/// Data item produced and consumed
#[derive(Debug, Clone)]
struct DataItem {
    id: u64,
    producer_id: usize,
    data: Vec<u8>,
    priority: Priority,
    created_at: Instant,
    metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Priority {
    Low = 1,
    Medium = 2,
    High = 3,
    Critical = 4,
}

/// Result of processing a data item
#[derive(Debug, Clone)]
struct ProcessedItem {
    id: u64,
    producer_id: usize,
    consumer_id: usize,
    processing_time: Duration,
    queue_time: Duration,
    result_data: HashMap<String, String>,
    created_at: Instant,
    processed_at: Instant,
}

/// Statistics for producers and consumers
#[derive(Debug, Default)]
struct ComponentStats {
    items_processed: AtomicU64,
    items_failed: AtomicU64,
    total_processing_time: AtomicU64,
    backpressure_events: AtomicU64,
    idle_time: AtomicU64,
    last_activity: AtomicU64,
}

impl ComponentStats {
    fn increment_processed(&self) {
        self.items_processed.fetch_add(1, Ordering::Relaxed);
        self.last_activity.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
            Ordering::Relaxed,
        );
    }
    
    fn increment_failed(&self) {
        self.items_failed.fetch_add(1, Ordering::Relaxed);
    }
    
    fn increment_backpressure(&self) {
        self.backpressure_events.fetch_add(1, Ordering::Relaxed);
    }
    
    fn add_processing_time(&self, duration: Duration) {
        self.total_processing_time.fetch_add(duration.as_millis() as u64, Ordering::Relaxed);
    }
    
    fn add_idle_time(&self, duration: Duration) {
        self.idle_time.fetch_add(duration.as_millis() as u64, Ordering::Relaxed);
    }
    
    fn get_stats(&self) -> (u64, u64, u64, u64, u64) {
        (
            self.items_processed.load(Ordering::Relaxed),
            self.items_failed.load(Ordering::Relaxed),
            self.total_processing_time.load(Ordering::Relaxed),
            self.backpressure_events.load(Ordering::Relaxed),
            self.idle_time.load(Ordering::Relaxed),
        )
    }
}

/// System-wide metrics
#[derive(Debug)]
struct SystemMetrics {
    producer_stats: Vec<ComponentStats>,
    consumer_stats: Vec<ComponentStats>,
    buffer_stats: ComponentStats,
    start_time: Instant,
    shutdown_signal: Arc<AtomicBool>,
}

impl SystemMetrics {
    fn new(num_producers: usize, num_consumers: usize) -> Self {
        Self {
            producer_stats: (0..num_producers).map(|_| ComponentStats::default()).collect(),
            consumer_stats: (0..num_consumers).map(|_| ComponentStats::default()).collect(),
            buffer_stats: ComponentStats::default(),
            start_time: Instant::now(),
            shutdown_signal: Arc::new(AtomicBool::new(false)),
        }
    }
    
    fn signal_shutdown(&self) {
        self.shutdown_signal.store(true, Ordering::Relaxed);
    }
    
    fn is_shutdown_signaled(&self) -> bool {
        self.shutdown_signal.load(Ordering::Relaxed)
    }
    
    fn print_summary(&self) {
        let total_time = self.start_time.elapsed();
        
        println!("\n=== Producer-Consumer System Summary ===");
        println!("Total Runtime: {:.2}s", total_time.as_secs_f64());
        
        // Producer statistics
        println!("\nProducer Statistics:");
        let mut total_produced = 0;
        let mut total_producer_backpressure = 0;
        
        for (i, stats) in self.producer_stats.iter().enumerate() {
            let (processed, failed, work_time, backpressure, _idle_time) = stats.get_stats();
            total_produced += processed;
            total_producer_backpressure += backpressure;
            
            let rate = processed as f64 / total_time.as_secs_f64();
            let avg_time = if processed > 0 { work_time / processed } else { 0 };
            
            println!("Producer {:2} | Produced: {:>6} | Failed: {:>4} | Rate: {:>6.1}/s | Backpressure: {:>4} | Avg: {:>4}ms",
                     i, processed, failed, rate, backpressure, avg_time);
        }
        
        // Consumer statistics
        println!("\nConsumer Statistics:");
        let mut total_consumed = 0;
        let mut total_consumer_backpressure = 0;
        
        for (i, stats) in self.consumer_stats.iter().enumerate() {
            let (processed, failed, work_time, backpressure, idle_time) = stats.get_stats();
            total_consumed += processed;
            total_consumer_backpressure += backpressure;
            
            let rate = processed as f64 / total_time.as_secs_f64();
            let avg_time = if processed > 0 { work_time / processed } else { 0 };
            let utilization = if work_time + idle_time > 0 {
                (work_time as f64 / (work_time + idle_time) as f64) * 100.0
            } else {
                0.0
            };
            
            println!("Consumer {:2} | Consumed: {:>6} | Failed: {:>4} | Rate: {:>6.1}/s | Util: {:>5.1}% | Avg: {:>4}ms",
                     i, processed, failed, rate, utilization, avg_time);
        }
        
        // Buffer statistics
        let (buffer_ops, buffer_failed, _buffer_time, buffer_backpressure, _) = self.buffer_stats.get_stats();
        
        println!("\nBuffer Statistics:");
        println!("Operations: {} | Failed: {} | Backpressure Events: {}", 
                 buffer_ops, buffer_failed, buffer_backpressure);
        
        // System-wide metrics
        println!("\nSystem Performance:");
        println!("Total Produced: {} | Total Consumed: {} | Buffer Efficiency: {:.2}%",
                 total_produced, total_consumed,
                 if total_produced > 0 { (total_consumed as f64 / total_produced as f64) * 100.0 } else { 0.0 });
        
        println!("Producer Backpressure: {} | Consumer Backpressure: {} | Total Backpressure: {}",
                 total_producer_backpressure, total_consumer_backpressure, 
                 total_producer_backpressure + total_consumer_backpressure);
        
        let overall_throughput = total_consumed as f64 / total_time.as_secs_f64();
        println!("Overall Throughput: {:.2} items/second", overall_throughput);
    }
}

/// Producer coroutine - generates data items
fn producer(
    producer_id: usize,
    config: ProducerConsumerConfig,
    buffer_tx: mpsc::Sender<DataItem>,
    metrics: Arc<SystemMetrics>,
) {
    println!("🔄 Starting Producer {}...", producer_id);
    
    let producer_stats = &metrics.producer_stats[producer_id];
    let target_rate = config.producer_rates.get(producer_id).copied().unwrap_or(100);
    let items_per_producer = config.total_items / config.num_producers;
    let production_interval = Duration::from_millis(1000 / target_rate);
    
    let mut produced_count = 0;
    let mut next_production_time = Instant::now();
    
    while produced_count < items_per_producer && !metrics.is_shutdown_signaled() {
        let production_start = Instant::now();
        
        // Rate limiting - wait until next production time
        if production_start < next_production_time {
            let wait_time = next_production_time - production_start;
            coroutine::sleep(wait_time);
            producer_stats.add_idle_time(wait_time);
        }
        
        // Create data item with varying characteristics
        let priority = match produced_count % 10 {
            0..=6 => Priority::Low,
            7..=8 => Priority::Medium,
            9 => Priority::High,
            _ => Priority::Critical,
        };
        
        let data_size = match priority {
            Priority::Low => 64,
            Priority::Medium => 128,
            Priority::High => 256,
            Priority::Critical => 512,
        };
        
        let mut metadata = HashMap::new();
        metadata.insert("producer_id".to_string(), producer_id.to_string());
        metadata.insert("sequence".to_string(), produced_count.to_string());
        metadata.insert("priority".to_string(), format!("{:?}", priority));
        metadata.insert("data_size".to_string(), data_size.to_string());
        
        let data_item = DataItem {
            id: (producer_id as u64 * 1_000_000) + produced_count as u64,
            producer_id,
            data: vec![0u8; data_size],
            priority,
            created_at: Instant::now(),
            metadata,
        };
        
        // Send item
        if let Err(_) = buffer_tx.send(data_item) {
            println!("❌ Producer {}: Buffer channel disconnected", producer_id);
            producer_stats.increment_failed();
            return;
        }
        
        produced_count += 1;
        producer_stats.increment_processed();
        producer_stats.add_processing_time(production_start.elapsed());
        
        // Update next production time
        next_production_time = Instant::now() + production_interval;
        
        // Progress reporting
        if produced_count % (items_per_producer / 10).max(1) == 0 {
            println!("📤 Producer {}: Produced {}/{} items", 
                     producer_id, produced_count, items_per_producer);
        }
    }
    
    println!("✅ Producer {} completed - {} items produced", producer_id, produced_count);
}

/// Consumer coroutine - processes data items
fn consumer(
    consumer_id: usize,
    config: ProducerConsumerConfig,
    buffer_rx: mpsc::Receiver<DataItem>,
    result_tx: mpsc::Sender<ProcessedItem>,
    metrics: Arc<SystemMetrics>,
) {
    println!("🔄 Starting Consumer {}...", consumer_id);
    
    let consumer_stats = &metrics.consumer_stats[consumer_id];
    let target_rate = config.consumer_rates.get(consumer_id).copied().unwrap_or(100);
    let min_processing_time = Duration::from_millis(1000 / target_rate);
    
    let mut consumed_count = 0;
    
    while !metrics.is_shutdown_signaled() {
        let receive_start = Instant::now();
        
        // Try to receive data item
        let data_item = match buffer_rx.recv() {
            Ok(item) => item,
            Err(_) => {
                println!("✅ Consumer {}: Buffer channel closed", consumer_id);
                break;
            }
        };
        
        let processing_start = Instant::now();
        let queue_time = processing_start.duration_since(data_item.created_at);
        
        // Simulate processing time based on priority and data size
        let base_processing_time = match data_item.priority {
            Priority::Low => min_processing_time,
            Priority::Medium => min_processing_time * 2,
            Priority::High => min_processing_time * 3,
            Priority::Critical => min_processing_time * 4,
        };
        
        let data_factor = (data_item.data.len() as f64 / 128.0).max(1.0);
        let actual_processing_time = Duration::from_millis(
            (base_processing_time.as_millis() as f64 * data_factor) as u64
        );
        
        coroutine::sleep(actual_processing_time);
        
        // Create processed result
        let mut result_data = HashMap::new();
        result_data.insert("consumer_id".to_string(), consumer_id.to_string());
        result_data.insert("original_producer".to_string(), data_item.producer_id.to_string());
        result_data.insert("priority".to_string(), format!("{:?}", data_item.priority));
        result_data.insert("data_size".to_string(), data_item.data.len().to_string());
        result_data.insert("queue_time_ms".to_string(), queue_time.as_millis().to_string());
        result_data.insert("processing_time_ms".to_string(), actual_processing_time.as_millis().to_string());
        
        // Add some computed results
        let checksum: u32 = data_item.data.iter().enumerate()
            .map(|(i, &b)| (i as u32 + b as u32) * (data_item.priority as u32))
            .sum();
        result_data.insert("checksum".to_string(), checksum.to_string());
        
        let processed_item = ProcessedItem {
            id: data_item.id,
            producer_id: data_item.producer_id,
            consumer_id,
            processing_time: actual_processing_time,
            queue_time,
            result_data,
            created_at: data_item.created_at,
            processed_at: Instant::now(),
        };
        
        // Send result
        if let Err(_) = result_tx.send(processed_item) {
            println!("❌ Consumer {}: Result channel disconnected", consumer_id);
            consumer_stats.increment_failed();
            return;
        }
        
        consumed_count += 1;
        consumer_stats.increment_processed();
        consumer_stats.add_processing_time(processing_start.elapsed());
        
        // Progress reporting
        if consumed_count % 100 == 0 {
            println!("📥 Consumer {}: Processed {} items", consumer_id, consumed_count);
        }
    }
    
    println!("✅ Consumer {} completed - {} items processed", consumer_id, consumed_count);
}

/// Result collector - aggregates processed results
fn result_collector(
    config: ProducerConsumerConfig,
    result_rx: mpsc::Receiver<ProcessedItem>,
    metrics: Arc<SystemMetrics>,
) {
    println!("🔄 Starting Result Collector...");
    
    let mut results = Vec::new();
    let mut priority_counts = HashMap::new();
    let mut producer_consumer_matrix = HashMap::new();
    
    while let Ok(result) = result_rx.recv() {
        // Collect statistics
        let priority_count = priority_counts.entry(format!("{:?}", 
            result.result_data.get("priority").unwrap_or(&"Unknown".to_string()))).or_insert(0);
        *priority_count += 1;
        
        let matrix_key = (result.producer_id, result.consumer_id);
        let matrix_count = producer_consumer_matrix.entry(matrix_key).or_insert(0);
        *matrix_count += 1;
        
        results.push(result);
        
        // Progress reporting
        if results.len() % (config.total_items / 10).max(1) == 0 {
            println!("📊 Collector: Collected {}/{} results", results.len(), config.total_items);
        }
    }
    
    // Final analysis
    println!("\n=== Result Analysis ===");
    println!("Total Results Collected: {}", results.len());
    
    // Priority distribution
    println!("\nPriority Distribution:");
    for (priority, count) in priority_counts.iter() {
        let percentage = (*count as f64 / results.len() as f64) * 100.0;
        println!("{}: {} ({:.1}%)", priority, count, percentage);
    }
    
    // Producer-Consumer matrix
    println!("\nProducer-Consumer Processing Matrix:");
    for ((producer_id, consumer_id), count) in producer_consumer_matrix.iter() {
        let percentage = (*count as f64 / results.len() as f64) * 100.0;
        println!("Producer {} → Consumer {}: {} ({:.1}%)", 
                 producer_id, consumer_id, count, percentage);
    }
    
    // Timing analysis
    if !results.is_empty() {
        let queue_times: Vec<Duration> = results.iter().map(|r| r.queue_time).collect();
        let processing_times: Vec<Duration> = results.iter().map(|r| r.processing_time).collect();
        
        let avg_queue_time = queue_times.iter().sum::<Duration>() / queue_times.len() as u32;
        let avg_processing_time = processing_times.iter().sum::<Duration>() / processing_times.len() as u32;
        
        let min_queue_time = queue_times.iter().min().unwrap();
        let max_queue_time = queue_times.iter().max().unwrap();
        
        println!("\nTiming Analysis:");
        println!("Average Queue Time: {:.2}ms | Min: {:.2}ms | Max: {:.2}ms",
                 avg_queue_time.as_secs_f64() * 1000.0,
                 min_queue_time.as_secs_f64() * 1000.0,
                 max_queue_time.as_secs_f64() * 1000.0);
        
        println!("Average Processing Time: {:.2}ms",
                 avg_processing_time.as_secs_f64() * 1000.0);
    }
    
    println!("✅ Result Collector completed - {} results processed", results.len());
}

/// Parse command line arguments
fn parse_args() -> ProducerConsumerConfig {
    let args: Vec<String> = std::env::args().collect();
    let mut config = ProducerConsumerConfig::default();
    
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--producers" => {
                if i + 1 < args.len() {
                    config.num_producers = args[i + 1].parse().unwrap_or(config.num_producers);
                    i += 1;
                }
            }
            "--consumers" => {
                if i + 1 < args.len() {
                    config.num_consumers = args[i + 1].parse().unwrap_or(config.num_consumers);
                    i += 1;
                }
            }
            "--total-items" => {
                if i + 1 < args.len() {
                    config.total_items = args[i + 1].parse().unwrap_or(config.total_items);
                    i += 1;
                }
            }
            "--disable-backpressure-logging" => {
                config.enable_backpressure_logging = false;
            }
            "--help" => {
                println!("Producer-Consumer Example");
                println!("Usage: cargo run --example producer_consumer_bounded [OPTIONS]");
                println!("Options:");
                println!("  --producers <N>                   Number of producer coroutines [default: 2]");
                println!("  --consumers <N>                   Number of consumer coroutines [default: 2]");
                println!("  --total-items <N>                 Total items to produce [default: 1000]");
                println!("  --disable-backpressure-logging    Disable backpressure event logging");
                println!("  --help                            Show this help message");
                std::process::exit(0);
            }
            _ => {}
        }
        i += 1;
    }
    
    config
}

fn main() {
    let config = parse_args();
    
    println!("🚀 Starting Producer-Consumer Example");
    println!("Configuration: {:?}", config);
    
    // Configure May runtime
    may::config().set_workers((config.num_producers + config.num_consumers).max(2));
    
    let start_time = Instant::now();
    let metrics = Arc::new(SystemMetrics::new(config.num_producers, config.num_consumers));
    
    // Run the producer-consumer system within a coroutine scope
    may::coroutine::scope(|scope| {
        // Create buffer channel
        let (buffer_tx, buffer_rx) = mpsc::channel();
        
        // Create result collection channel
        let (result_tx, result_rx) = mpsc::channel();
        
        // Spawn producer coroutines
        for producer_id in 0..config.num_producers {
            let buffer_tx_clone = buffer_tx.clone();
            let metrics_clone = metrics.clone();
            go!(scope, move || {
                producer(producer_id, config, buffer_tx_clone, metrics_clone);
            });
        }
        
        // Drop original buffer sender so consumers know when to stop
        drop(buffer_tx);
        
        // Spawn consumer coroutines - each gets its own receiver
        let mut buffer_receivers = Vec::new();
        let mut consumer_receivers = Vec::new();
        
        for _ in 0..config.num_consumers {
            let (tx, rx) = mpsc::channel();
            buffer_receivers.push(tx);
            consumer_receivers.push(rx);
        }
        
        // Create a distributor to send items to consumers
        let metrics_clone = metrics.clone();
        go!(scope, move || {
            let mut next_consumer = 0;
            while let Ok(item) = buffer_rx.recv() {
                let target_consumer = next_consumer % config.num_consumers;
                if let Err(_) = buffer_receivers[target_consumer].send(item) {
                    break;
                }
                next_consumer += 1;
            }
        });
        
        for consumer_id in 0..config.num_consumers {
            let consumer_rx = consumer_receivers.pop().unwrap();
            let result_tx_clone = result_tx.clone();
            let metrics_clone = metrics.clone();
            go!(scope, move || {
                consumer(consumer_id, config, consumer_rx, result_tx_clone, metrics_clone);
            });
        }
        
        // Drop original result sender so collector knows when to stop
        drop(result_tx);
        
        // Spawn result collector
        let metrics_clone = metrics.clone();
        go!(scope, move || {
            result_collector(config, result_rx, metrics_clone);
        });
        
        // System monitor
        let metrics_clone = metrics.clone();
        go!(scope, move || {
            let mut last_total = 0;
            loop {
                coroutine::sleep(Duration::from_secs(3));
                
                let total_produced: u64 = metrics_clone.producer_stats.iter()
                    .map(|s| s.items_processed.load(Ordering::Relaxed))
                    .sum();
                
                let total_consumed: u64 = metrics_clone.consumer_stats.iter()
                    .map(|s| s.items_processed.load(Ordering::Relaxed))
                    .sum();
                
                if total_consumed >= config.total_items as u64 {
                    metrics_clone.signal_shutdown();
                    break;
                }
                
                let rate = (total_consumed - last_total) as f64 / 3.0;
                println!("📊 System: Produced: {} | Consumed: {} | Rate: {:.1}/s", 
                         total_produced, total_consumed, rate);
                last_total = total_consumed;
            }
        });
    });
    
    let total_time = start_time.elapsed();
    
    // Print comprehensive metrics
    metrics.print_summary();
    
    println!("\n✨ Producer-Consumer Example completed successfully!");
    println!("🎯 Total execution time: {:.2}s", total_time.as_secs_f64());
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_config_default() {
        let config = ProducerConsumerConfig::default();
        assert_eq!(config.num_producers, 2);
        assert_eq!(config.num_consumers, 2);
        assert_eq!(config.total_items, 1000);
    }
    
    #[test]
    fn test_component_stats() {
        let stats = ComponentStats::default();
        
        stats.increment_processed();
        stats.increment_processed();
        stats.increment_failed();
        stats.increment_backpressure();
        stats.add_processing_time(Duration::from_millis(100));
        stats.add_idle_time(Duration::from_millis(50));
        
        let (processed, failed, work_time, backpressure, idle_time) = stats.get_stats();
        assert_eq!(processed, 2);
        assert_eq!(failed, 1);
        assert_eq!(work_time, 100);
        assert_eq!(backpressure, 1);
        assert_eq!(idle_time, 50);
    }
    
    #[test]
    fn test_priority_ordering() {
        assert!(Priority::Critical > Priority::High);
        assert!(Priority::High > Priority::Medium);
        assert!(Priority::Medium > Priority::Low);
    }
    
    #[test]
    fn test_small_producer_consumer() {
        may::config().set_workers(2);
        
        let config = ProducerConsumerConfig {
            num_producers: 1,
            num_consumers: 1,
            total_items: 10,
            producer_rates: [1000, 0, 0, 0],
            consumer_rates: [1000, 0, 0, 0],
            enable_backpressure_logging: false,
            shutdown_timeout_secs: 5,
        };
        
        let metrics = Arc::new(SystemMetrics::new(config.num_producers, config.num_consumers));
        
        may::coroutine::scope(|scope| {
            let (buffer_tx, buffer_rx) = mpsc::channel();
            let (result_tx, result_rx) = mpsc::channel();
            
            let buffer_tx_clone = buffer_tx.clone();
            let metrics_clone = metrics.clone();
            go!(scope, move || {
                producer(0, config, buffer_tx_clone, metrics_clone);
            });
            
            drop(buffer_tx);
            
            let result_tx_clone = result_tx.clone();
            let metrics_clone = metrics.clone();
            go!(scope, move || {
                consumer(0, config, buffer_rx, result_tx_clone, metrics_clone);
            });
            
            drop(result_tx);
            
            let metrics_clone = metrics.clone();
            go!(scope, move || {
                result_collector(config, result_rx, metrics_clone);
            });
        });
        
        // Verify processing completed
        let produced = metrics.producer_stats[0].items_processed.load(Ordering::Relaxed);
        let consumed = metrics.consumer_stats[0].items_processed.load(Ordering::Relaxed);
        
        assert_eq!(produced, 10);
        assert_eq!(consumed, 10);
    }
} 