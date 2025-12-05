/// # Fan-Out/Fan-In Pattern Example
///
/// ## Description
/// This example demonstrates the fan-out/fan-in pattern using May coroutines.
/// Work is distributed across multiple worker coroutines (fan-out) and results
/// are aggregated back into a single stream (fan-in).
///
/// ## Architecture
/// ```text
/// [Work Queue] → [Distributor] → [Worker 1] ┐
///                              → [Worker 2] ├─→ [Aggregator] → [Results]
///                              → [Worker 3] ┘
/// ```
///
/// ## Use Cases
/// - Parallel processing of independent tasks
/// - Load balancing across multiple workers
/// - Map-reduce style computations
/// - Concurrent API calls or database operations
///
/// ## Performance Characteristics
/// - Configurable number of worker coroutines
/// - Work distribution across multiple workers
/// - Graceful handling of worker failures
/// - Comprehensive metrics and monitoring
///
/// ## Usage
/// ```bash
/// cargo run --example fan_out_fan_in
/// cargo run --example fan_out_fan_in -- --workers 8 --tasks 1000 --work-complexity 100
/// ```

#[macro_use]
extern crate may;

use may::coroutine;
use may::sync::mpsc;
use rand;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Configuration for the fan-out/fan-in example
#[derive(Debug, Clone, Copy)]
struct FanOutConfig {
    num_workers: usize,
    num_tasks: usize,
    work_complexity: u64,
    enable_failures: bool,
    failure_rate: f64,
    work_distribution_strategy: WorkDistributionStrategy,
}

#[derive(Debug, Clone, Copy)]
enum WorkDistributionStrategy {
    RoundRobin,
    Random,
}

impl Default for FanOutConfig {
    fn default() -> Self {
        Self {
            num_workers: 4,
            num_tasks: 100,
            work_complexity: 50,
            enable_failures: false,
            failure_rate: 0.02,
            work_distribution_strategy: WorkDistributionStrategy::RoundRobin,
        }
    }
}

/// Work item to be processed by workers
#[derive(Debug, Clone)]
struct WorkItem {
    id: u64,
    data: Vec<u8>,
    complexity: u64,
    created_at: Instant,
}

/// Result of processing a work item
#[derive(Debug, Clone)]
struct WorkResult {
    id: u64,
    worker_id: usize,
    result_data: HashMap<String, String>,
    processing_time: Duration,
    created_at: Instant,
    completed_at: Instant,
}

/// Worker statistics for monitoring
#[derive(Debug, Default)]
struct WorkerStats {
    tasks_processed: AtomicU64,
    tasks_failed: AtomicU64,
    total_processing_time: AtomicU64,
    idle_time: AtomicU64,
}

impl WorkerStats {
    fn increment_processed(&self) {
        self.tasks_processed.fetch_add(1, Ordering::Relaxed);
    }

    fn increment_failed(&self) {
        self.tasks_failed.fetch_add(1, Ordering::Relaxed);
    }

    fn add_processing_time(&self, duration: Duration) {
        self.total_processing_time
            .fetch_add(duration.as_millis() as u64, Ordering::Relaxed);
    }

    fn add_idle_time(&self, duration: Duration) {
        self.idle_time
            .fetch_add(duration.as_millis() as u64, Ordering::Relaxed);
    }

    fn get_stats(&self) -> (u64, u64, u64, u64) {
        (
            self.tasks_processed.load(Ordering::Relaxed),
            self.tasks_failed.load(Ordering::Relaxed),
            self.total_processing_time.load(Ordering::Relaxed),
            self.idle_time.load(Ordering::Relaxed),
        )
    }
}

/// Overall system metrics
#[derive(Debug)]
struct SystemMetrics {
    worker_stats: Vec<WorkerStats>,
    distributor_stats: WorkerStats,
    aggregator_stats: WorkerStats,
    start_time: Instant,
}

impl SystemMetrics {
    fn new(num_workers: usize) -> Self {
        Self {
            worker_stats: (0..num_workers).map(|_| WorkerStats::default()).collect(),
            distributor_stats: WorkerStats::default(),
            aggregator_stats: WorkerStats::default(),
            start_time: Instant::now(),
        }
    }

    fn print_summary(&self) {
        let total_time = self.start_time.elapsed();

        println!("\n=== Fan-Out/Fan-In Processing Summary ===");
        println!("Total Runtime: {:.2}s", total_time.as_secs_f64());

        // Worker statistics
        println!("\nWorker Statistics:");
        let mut total_processed = 0;
        let mut total_failed = 0;
        let mut total_work_time = 0;

        for (i, stats) in self.worker_stats.iter().enumerate() {
            let (processed, failed, work_time, idle_time) = stats.get_stats();
            total_processed += processed;
            total_failed += failed;
            total_work_time += work_time;

            let avg_time = if processed > 0 {
                work_time / processed
            } else {
                0
            };
            let utilization = if work_time + idle_time > 0 {
                (work_time as f64 / (work_time + idle_time) as f64) * 100.0
            } else {
                0.0
            };

            println!(
                "Worker {:2} | Processed: {:>6} | Failed: {:>4} | Avg: {:>4}ms | Util: {:>5.1}%",
                i, processed, failed, avg_time, utilization
            );
        }

        // Aggregated statistics
        let (dist_processed, dist_failed, dist_time, _) = self.distributor_stats.get_stats();
        let (agg_processed, agg_failed, agg_time, _) = self.aggregator_stats.get_stats();

        println!("\nSystem Statistics:");
        println!(
            "Distributor   | Processed: {:>6} | Failed: {:>4} | Total Time: {:>6}ms",
            dist_processed, dist_failed, dist_time
        );
        println!(
            "Aggregator    | Processed: {:>6} | Failed: {:>4} | Total Time: {:>6}ms",
            agg_processed, agg_failed, agg_time
        );

        // Overall metrics
        let success_rate = if total_processed + total_failed > 0 {
            (total_processed as f64 / (total_processed + total_failed) as f64) * 100.0
        } else {
            0.0
        };

        let throughput = total_processed as f64 / total_time.as_secs_f64();
        let avg_worker_time = if total_processed > 0 {
            total_work_time / total_processed
        } else {
            0
        };

        println!("\nOverall Performance:");
        println!(
            "Success Rate: {:.2}% | Throughput: {:.2} tasks/sec | Avg Processing: {}ms",
            success_rate, throughput, avg_worker_time
        );
    }
}

/// Work distributor - fans out work to multiple workers
fn work_distributor(
    config: FanOutConfig,
    work_senders: Vec<mpsc::Sender<WorkItem>>,
    metrics: Arc<SystemMetrics>,
) {
    println!("🔄 Starting Work Distributor...");

    let mut next_worker = 0;
    let mut work_count = 0;

    // Create work items
    for i in 0..config.num_tasks {
        let start_time = Instant::now();

        // Create work item with varying complexity
        let complexity = config.work_complexity + (i as u64 % 50);
        let data_size = (complexity / 10) as usize;

        let work_item = WorkItem {
            id: i as u64,
            data: vec![0u8; data_size],
            complexity,
            created_at: start_time,
        };

        // Distribute work based on strategy
        let target_worker = match config.work_distribution_strategy {
            WorkDistributionStrategy::RoundRobin => {
                let worker = next_worker;
                next_worker = (next_worker + 1) % config.num_workers;
                worker
            }
            WorkDistributionStrategy::Random => rand::random::<usize>() % config.num_workers,
        };

        // Send work to selected worker
        if let Err(_) = work_senders[target_worker].send(work_item) {
            println!("❌ Distributor: Worker {} disconnected", target_worker);
            metrics.distributor_stats.increment_failed();
            continue;
        }

        work_count += 1;
        metrics.distributor_stats.increment_processed();
        metrics
            .distributor_stats
            .add_processing_time(start_time.elapsed());

        // Progress reporting
        if work_count % (config.num_tasks / 10).max(1) == 0 {
            println!(
                "📤 Distributor: Sent {}/{} work items",
                work_count, config.num_tasks
            );
        }
    }

    // Signal completion by closing all work channels
    for sender in work_senders {
        drop(sender);
    }

    println!(
        "✅ Work Distributor completed - {} items distributed",
        work_count
    );
}

/// Worker coroutine - processes work items
fn worker(
    worker_id: usize,
    config: FanOutConfig,
    work_rx: mpsc::Receiver<WorkItem>,
    result_tx: mpsc::Sender<WorkResult>,
    metrics: Arc<SystemMetrics>,
) {
    println!("🔄 Starting Worker {}...", worker_id);

    let worker_stats = &metrics.worker_stats[worker_id];
    let mut processed_count = 0;

    while let Ok(work_item) = work_rx.recv() {
        let processing_start = Instant::now();

        // Simulate work processing
        let work_duration = Duration::from_millis(work_item.complexity);
        coroutine::sleep(work_duration);

        // Simulate processing failures
        if config.enable_failures && rand::random::<f64>() < config.failure_rate {
            println!(
                "⚠️  Worker {}: Failed to process item {}",
                worker_id, work_item.id
            );
            worker_stats.increment_failed();
            continue;
        }

        // Process the work item
        let mut result_data = HashMap::new();
        result_data.insert("worker_id".to_string(), worker_id.to_string());
        result_data.insert("data_size".to_string(), work_item.data.len().to_string());
        result_data.insert("complexity".to_string(), work_item.complexity.to_string());
        result_data.insert(
            "processing_time_ms".to_string(),
            work_duration.as_millis().to_string(),
        );

        // Add some computed results
        let checksum: u32 = work_item
            .data
            .iter()
            .enumerate()
            .map(|(i, &b)| (i as u32 + b as u32) * work_item.complexity as u32)
            .sum();
        result_data.insert("checksum".to_string(), checksum.to_string());

        let work_result = WorkResult {
            id: work_item.id,
            worker_id,
            result_data,
            processing_time: processing_start.elapsed(),
            created_at: work_item.created_at,
            completed_at: Instant::now(),
        };

        // Send result
        if let Err(_) = result_tx.send(work_result) {
            println!("❌ Worker {}: Result channel disconnected", worker_id);
            worker_stats.increment_failed();
            return;
        }

        processed_count += 1;
        worker_stats.increment_processed();
        worker_stats.add_processing_time(processing_start.elapsed());

        // Periodic progress reporting
        if processed_count % 50 == 0 {
            println!(
                "�� Worker {}: Processed {} items",
                worker_id, processed_count
            );
        }
    }

    println!(
        "✅ Worker {} completed - {} items processed",
        worker_id, processed_count
    );
}

/// Result aggregator - fans in results from all workers
fn result_aggregator(
    config: FanOutConfig,
    result_rx: mpsc::Receiver<WorkResult>,
    metrics: Arc<SystemMetrics>,
) {
    println!("🔄 Starting Result Aggregator...");

    let mut results = Vec::new();
    let mut worker_counts = HashMap::new();
    let mut total_processing_time = Duration::new(0, 0);

    while let Ok(result) = result_rx.recv() {
        let start_time = Instant::now();

        // Aggregate result statistics
        let worker_count = worker_counts.entry(result.worker_id).or_insert(0);
        *worker_count += 1;

        total_processing_time += result.processing_time;

        // Store result for final analysis
        results.push(result);

        metrics.aggregator_stats.increment_processed();
        metrics
            .aggregator_stats
            .add_processing_time(start_time.elapsed());

        // Progress reporting
        if results.len() % (config.num_tasks / 10).max(1) == 0 {
            println!(
                "📥 Aggregator: Collected {}/{} results",
                results.len(),
                config.num_tasks
            );
        }
    }

    // Final result analysis
    println!("\n=== Result Analysis ===");
    println!("Total Results Collected: {}", results.len());

    // Worker distribution analysis
    println!("\nWork Distribution:");
    for (worker_id, count) in worker_counts.iter() {
        let percentage = (*count as f64 / results.len() as f64) * 100.0;
        println!("Worker {}: {} tasks ({:.1}%)", worker_id, count, percentage);
    }

    // Timing analysis
    if !results.is_empty() {
        let avg_processing_time = total_processing_time / results.len() as u32;
        let min_time = results.iter().map(|r| r.processing_time).min().unwrap();
        let max_time = results.iter().map(|r| r.processing_time).max().unwrap();

        println!("\nProcessing Time Analysis:");
        println!(
            "Average: {:.2}ms | Min: {:.2}ms | Max: {:.2}ms",
            avg_processing_time.as_secs_f64() * 1000.0,
            min_time.as_secs_f64() * 1000.0,
            max_time.as_secs_f64() * 1000.0
        );
    }

    // End-to-end latency analysis
    let end_to_end_times: Vec<Duration> = results
        .iter()
        .map(|r| r.completed_at.duration_since(r.created_at))
        .collect();

    if !end_to_end_times.is_empty() {
        let avg_e2e = end_to_end_times.iter().sum::<Duration>() / end_to_end_times.len() as u32;
        let min_e2e = end_to_end_times.iter().min().unwrap();
        let max_e2e = end_to_end_times.iter().max().unwrap();

        println!("\nEnd-to-End Latency:");
        println!(
            "Average: {:.2}ms | Min: {:.2}ms | Max: {:.2}ms",
            avg_e2e.as_secs_f64() * 1000.0,
            min_e2e.as_secs_f64() * 1000.0,
            max_e2e.as_secs_f64() * 1000.0
        );
    }

    println!(
        "✅ Result Aggregator completed - {} results processed",
        results.len()
    );
}

/// Parse command line arguments
fn parse_args() -> FanOutConfig {
    let args: Vec<String> = std::env::args().collect();
    let mut config = FanOutConfig::default();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--workers" => {
                if i + 1 < args.len() {
                    config.num_workers = args[i + 1].parse().unwrap_or(config.num_workers);
                    i += 1;
                }
            }
            "--tasks" => {
                if i + 1 < args.len() {
                    config.num_tasks = args[i + 1].parse().unwrap_or(config.num_tasks);
                    i += 1;
                }
            }
            "--work-complexity" => {
                if i + 1 < args.len() {
                    config.work_complexity = args[i + 1].parse().unwrap_or(config.work_complexity);
                    i += 1;
                }
            }
            "--enable-failures" => {
                config.enable_failures = true;
            }
            "--failure-rate" => {
                if i + 1 < args.len() {
                    config.failure_rate = args[i + 1].parse().unwrap_or(config.failure_rate);
                    i += 1;
                }
            }
            "--strategy" => {
                if i + 1 < args.len() {
                    config.work_distribution_strategy = match args[i + 1].as_str() {
                        "round-robin" => WorkDistributionStrategy::RoundRobin,
                        "random" => WorkDistributionStrategy::Random,
                        _ => config.work_distribution_strategy,
                    };
                    i += 1;
                }
            }
            "--help" => {
                println!("Fan-Out/Fan-In Pattern Example");
                println!("Usage: cargo run --example fan_out_fan_in [OPTIONS]");
                println!("Options:");
                println!("  --workers <N>           Number of worker coroutines [default: 4]");
                println!("  --tasks <N>             Number of tasks to process [default: 100]");
                println!("  --work-complexity <N>   Work complexity (processing time in ms) [default: 50]");
                println!("  --enable-failures       Enable random failures in workers");
                println!("  --failure-rate <RATE>   Failure rate (0.0-1.0) [default: 0.02]");
                println!("  --strategy <STRATEGY>   Distribution strategy: round-robin, random [default: round-robin]");
                println!("  --help                  Show this help message");
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

    println!("🚀 Starting Fan-Out/Fan-In Pattern Example");
    println!("Configuration: {:?}", config);

    // Configure May runtime
    may::config().set_workers(config.num_workers.max(2));

    let start_time = Instant::now();
    let metrics = Arc::new(SystemMetrics::new(config.num_workers));

    // Run the fan-out/fan-in pattern within a coroutine scope
    may::coroutine::scope(|scope| {
        // Create channels for work distribution
        let mut work_senders = Vec::new();
        let mut work_receivers = Vec::new();

        for _ in 0..config.num_workers {
            let (tx, rx) = mpsc::channel();
            work_senders.push(tx);
            work_receivers.push(rx);
        }

        // Create channel for result aggregation
        let (result_tx, result_rx) = mpsc::channel();

        // Spawn work distributor
        let work_senders_clone = work_senders.clone();
        let metrics_clone = metrics.clone();
        go!(scope, move || {
            work_distributor(config, work_senders_clone, metrics_clone);
        });

        // Spawn worker coroutines
        for worker_id in 0..config.num_workers {
            let work_rx = work_receivers.pop().unwrap();
            let result_tx_clone = result_tx.clone();
            let metrics_clone = metrics.clone();
            go!(scope, move || {
                worker(worker_id, config, work_rx, result_tx_clone, metrics_clone);
            });
        }

        // Drop the original result sender so aggregator knows when to stop
        drop(result_tx);

        // Spawn result aggregator
        let metrics_clone = metrics.clone();
        go!(scope, move || {
            result_aggregator(config, result_rx, metrics_clone);
        });

        // Progress monitor
        let metrics_clone = metrics.clone();
        go!(scope, move || {
            let mut last_completed = 0;
            loop {
                coroutine::sleep(Duration::from_secs(2));

                let completed = metrics_clone
                    .aggregator_stats
                    .tasks_processed
                    .load(Ordering::Relaxed);
                if completed >= config.num_tasks as u64 {
                    break;
                }

                if completed > last_completed {
                    let rate = (completed - last_completed) as f64 / 2.0;
                    println!(
                        "📊 Progress: {}/{} tasks completed ({:.1} tasks/sec)",
                        completed, config.num_tasks, rate
                    );
                    last_completed = completed;
                }
            }
        });
    });

    let total_time = start_time.elapsed();

    // Print comprehensive metrics
    metrics.print_summary();

    println!("\n✨ Fan-Out/Fan-In Pattern Example completed successfully!");
    println!("🎯 Total execution time: {:.2}s", total_time.as_secs_f64());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fan_out_config_default() {
        let config = FanOutConfig::default();
        assert_eq!(config.num_workers, 4);
        assert_eq!(config.num_tasks, 100);
        assert_eq!(config.work_complexity, 50);
        assert!(!config.enable_failures);
    }

    #[test]
    fn test_worker_stats() {
        let stats = WorkerStats::default();

        stats.increment_processed();
        stats.increment_processed();
        stats.increment_failed();
        stats.add_processing_time(Duration::from_millis(100));
        stats.add_idle_time(Duration::from_millis(50));

        let (processed, failed, work_time, idle_time) = stats.get_stats();
        assert_eq!(processed, 2);
        assert_eq!(failed, 1);
        assert_eq!(work_time, 100);
        assert_eq!(idle_time, 50);
    }

    #[test]
    fn test_small_fan_out_fan_in() {
        may::config().set_workers(2);

        let config = FanOutConfig {
            num_workers: 2,
            num_tasks: 5,
            work_complexity: 1,
            enable_failures: false,
            failure_rate: 0.0,
            work_distribution_strategy: WorkDistributionStrategy::RoundRobin,
        };

        let metrics = Arc::new(SystemMetrics::new(config.num_workers));

        may::coroutine::scope(|scope| {
            let mut work_senders = Vec::new();
            let mut work_receivers = Vec::new();

            for _ in 0..config.num_workers {
                let (tx, rx) = mpsc::channel();
                work_senders.push(tx);
                work_receivers.push(rx);
            }

            let (result_tx, result_rx) = mpsc::channel();

            let work_senders_clone = work_senders.clone();
            let metrics_clone = metrics.clone();
            go!(scope, move || {
                work_distributor(config, work_senders_clone, metrics_clone);
            });

            for worker_id in 0..config.num_workers {
                let work_rx = work_receivers.pop().unwrap();
                let result_tx_clone = result_tx.clone();
                let metrics_clone = metrics.clone();
                go!(scope, move || {
                    worker(worker_id, config, work_rx, result_tx_clone, metrics_clone);
                });
            }

            drop(result_tx);

            let metrics_clone = metrics.clone();
            go!(scope, move || {
                result_aggregator(config, result_rx, metrics_clone);
            });
        });

        // Verify all tasks were processed
        let completed = metrics
            .aggregator_stats
            .tasks_processed
            .load(Ordering::Relaxed);
        assert_eq!(completed, 5);
    }
}
