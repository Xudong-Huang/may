// Example code - allow some clippy lints for demonstration clarity
#![allow(clippy::uninlined_format_args)]
#![allow(clippy::redundant_pattern_matching)]
#![allow(clippy::manual_is_multiple_of)]
#![allow(clippy::collapsible_else_if)]
#![allow(dead_code)]
#![allow(unused_variables)]

//! # Pipeline Data Processing Example
//!
//! ## Description
//! This example demonstrates a multi-stage data processing pipeline using May coroutines.
//! Data flows through multiple transformation stages: Reader → Parser → Transformer → Validator → Writer.
//! Each stage runs concurrently with proper coordination and error handling.
//!
//! ## Architecture
//! ```text
//! [Data Source] → [Reader] → [Parser] → [Transformer] → [Validator] → [Writer] → [Output]
//!                     ↓         ↓           ↓            ↓          ↓
//!                 [Channel]  [Channel]   [Channel]    [Channel]   [Channel]
//! ```
//!
//! ## Performance Characteristics
//! - Concurrent processing across all pipeline stages
//! - Proper coordination between stages
//! - Error propagation with graceful degradation
//! - Comprehensive metrics and monitoring
//!
//! ## Usage
//! ```bash
//! cargo run --example pipeline_data_processing
//! cargo run --example pipeline_data_processing -- --input-size 10000
//! ```

#[macro_use]
extern crate may;

use may::coroutine;
use may::sync::mpsc;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
struct UserRecord {
    id: u64,
    name: String,
    email: String,
    age: u32,
    country: String,
    subscription_type: String,
    last_login: String,
    credits: u32,
}

/// Configuration for the pipeline processing example
#[derive(Debug, Clone)]
struct PipelineConfig {
    input_size: usize,
    processing_delay_ms: u64,
    enable_errors: bool,
    error_rate: f64,
    input_file: Option<String>,
    data_source: DataSource,
}

#[derive(Debug, Clone)]
enum DataSource {
    Generated,
    CsvFile(String),
    JsonFile(String),
    TextFile(String),
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            input_size: 1000,
            processing_delay_ms: 0, // No delay by default for faster testing
            enable_errors: false,
            error_rate: 0.05, // 5% error rate
            input_file: None,
            data_source: DataSource::Generated,
        }
    }
}

/// Raw data item entering the pipeline
#[derive(Debug, Clone)]
struct RawData {
    id: u64,
    content: String,
    timestamp: u64,
}

/// Parsed data after initial processing
#[derive(Debug, Clone)]
struct ParsedData {
    id: u64,
    fields: HashMap<String, String>,
    timestamp: u64,
}

/// Transformed data after business logic processing
#[derive(Debug, Clone)]
struct TransformedData {
    id: u64,
    processed_fields: HashMap<String, String>,
    score: f64,
    timestamp: u64,
}

/// Validated data ready for output
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields are part of the data model
struct ValidatedData {
    id: u64,
    final_data: HashMap<String, String>,
    score: f64,
    timestamp: u64,
    validation_status: String,
}

/// Pipeline stage metrics for monitoring
#[derive(Debug, Default)]
struct StageMetrics {
    processed: AtomicU64,
    errors: AtomicU64,
    processing_time_ms: AtomicU64,
}

impl StageMetrics {
    fn increment_processed(&self) {
        self.processed.fetch_add(1, Ordering::Relaxed);
    }

    fn increment_errors(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }

    fn add_processing_time(&self, duration: Duration) {
        self.processing_time_ms
            .fetch_add(duration.as_millis() as u64, Ordering::Relaxed);
    }

    fn get_stats(&self) -> (u64, u64, u64) {
        (
            self.processed.load(Ordering::Relaxed),
            self.errors.load(Ordering::Relaxed),
            self.processing_time_ms.load(Ordering::Relaxed),
        )
    }
}

/// Pipeline metrics collection
#[derive(Debug)]
struct PipelineMetrics {
    reader: StageMetrics,
    parser: StageMetrics,
    transformer: StageMetrics,
    validator: StageMetrics,
    writer: StageMetrics,
}

impl PipelineMetrics {
    fn new() -> Self {
        Self {
            reader: StageMetrics::default(),
            parser: StageMetrics::default(),
            transformer: StageMetrics::default(),
            validator: StageMetrics::default(),
            writer: StageMetrics::default(),
        }
    }

    fn print_summary(&self) {
        println!("\n=== Pipeline Processing Summary ===");

        let stages = [
            ("Reader", &self.reader),
            ("Parser", &self.parser),
            ("Transformer", &self.transformer),
            ("Validator", &self.validator),
            ("Writer", &self.writer),
        ];

        for (name, metrics) in stages.iter() {
            let (processed, errors, time_ms) = metrics.get_stats();
            let avg_time = if processed > 0 {
                time_ms / processed
            } else {
                0
            };
            println!(
                "{:<12} | Processed: {:>6} | Errors: {:>4} | Avg Time: {:>4}ms",
                name, processed, errors, avg_time
            );
        }

        let total_processed = self.writer.processed.load(Ordering::Relaxed);
        let total_errors: u64 = [
            &self.reader,
            &self.parser,
            &self.transformer,
            &self.validator,
            &self.writer,
        ]
        .iter()
        .map(|m| m.errors.load(Ordering::Relaxed))
        .sum();

        println!(
            "Total Processed: {} | Total Errors: {} | Success Rate: {:.2}%",
            total_processed,
            total_errors,
            if total_processed > 0 {
                (total_processed as f64 / (total_processed + total_errors) as f64) * 100.0
            } else {
                0.0
            }
        );
    }
}

/// Dynamic record generator that creates user records on-the-fly
struct RecordGenerator {
    count: u64,
    first_names: Vec<&'static str>,
    last_names: Vec<&'static str>,
    countries: Vec<&'static str>,
    subscription_types: Vec<&'static str>,
}

impl RecordGenerator {
    fn new() -> Self {
        Self {
            count: 0,
            first_names: vec![
                "Alice", "Bob", "Charlie", "Diana", "Eve", "Frank", "Grace", "Henry", "Ivy",
                "Jack", "Kate", "Liam", "Mia", "Noah", "Olivia", "Paul", "Quinn", "Rachel", "Sam",
                "Tina", "Uma", "Victor", "Wendy", "Xavier", "Yuki", "Zoe", "Alex", "Beth", "Chris",
                "Dana", "Eli", "Fiona", "George", "Hannah", "Ian", "Julia", "Kevin", "Luna", "Max",
                "Nina", "Oscar", "Penny", "Quincy", "Rose", "Steve", "Tara", "Uri", "Vera", "Will",
                "Xara",
            ],
            last_names: vec![
                "Johnson",
                "Smith",
                "Brown",
                "Prince",
                "Wilson",
                "Miller",
                "Lee",
                "Davis",
                "Chen",
                "Taylor",
                "White",
                "Garcia",
                "Rodriguez",
                "Martinez",
                "Anderson",
                "Thomas",
                "Jackson",
                "Moore",
                "Martin",
                "Thompson",
                "Harris",
                "Clark",
                "Lewis",
                "Robinson",
                "Walker",
                "Hall",
                "Allen",
                "Young",
                "King",
                "Wright",
                "Lopez",
                "Hill",
                "Scott",
                "Green",
                "Adams",
                "Baker",
                "Gonzalez",
                "Nelson",
                "Carter",
                "Mitchell",
                "Perez",
                "Roberts",
                "Turner",
                "Phillips",
                "Campbell",
                "Parker",
                "Evans",
                "Edwards",
                "Collins",
                "Stewart",
            ],
            countries: vec![
                "USA",
                "Canada",
                "UK",
                "Australia",
                "Germany",
                "France",
                "Japan",
                "Brazil",
                "China",
                "India",
                "Russia",
                "Spain",
                "Mexico",
                "Italy",
                "Netherlands",
                "Sweden",
                "Norway",
                "Denmark",
                "Finland",
                "Belgium",
                "Switzerland",
                "Austria",
                "Poland",
                "Czech Republic",
                "Hungary",
                "Portugal",
                "Greece",
                "Ireland",
                "New Zealand",
                "South Korea",
                "Singapore",
                "Malaysia",
                "Thailand",
                "Vietnam",
                "Philippines",
                "Indonesia",
                "Turkey",
                "Israel",
                "South Africa",
                "Egypt",
                "Nigeria",
                "Kenya",
                "Morocco",
                "Argentina",
                "Chile",
                "Colombia",
                "Peru",
                "Uruguay",
                "Venezuela",
                "Ecuador",
            ],
            subscription_types: vec!["Basic", "Premium", "Enterprise"],
        }
    }

    fn generate_record(&mut self) -> UserRecord {
        self.count += 1;
        let id = self.count;

        // Use simple pseudo-random based on ID for deterministic results
        let first_idx = (id * 17) % self.first_names.len() as u64;
        let last_idx = (id * 23) % self.last_names.len() as u64;
        let country_idx = (id * 31) % self.countries.len() as u64;
        let sub_idx = (id * 7) % self.subscription_types.len() as u64;

        let first_name = self.first_names[first_idx as usize];
        let last_name = self.last_names[last_idx as usize];
        let country = self.countries[country_idx as usize];
        let subscription_type = self.subscription_types[sub_idx as usize];

        let name = format!("{} {}", first_name, last_name);
        let email = format!(
            "{}.{}{}@email.com",
            first_name.to_lowercase(),
            last_name.to_lowercase(),
            id
        );
        let age = 18 + ((id * 11) % 48) as u32; // Age between 18-65
        let last_login = format!("2024-01-{:02}", 1 + (id % 30)); // Random day in January

        let credits = match subscription_type {
            "Basic" => 100 + ((id * 13) % 900) as u32,
            "Premium" => 1000 + ((id * 19) % 2000) as u32,
            "Enterprise" => 3000 + ((id * 29) % 7000) as u32,
            _ => 500,
        };

        UserRecord {
            id,
            name,
            email,
            age,
            country: country.to_string(),
            subscription_type: subscription_type.to_string(),
            last_login,
            credits,
        }
    }
}

/// Data Reader Stage - Generates or reads input data
fn data_reader_stage(
    config: PipelineConfig,
    output_tx: mpsc::Sender<RawData>,
    metrics: Arc<PipelineMetrics>,
) {
    println!("🔄 Starting Data Reader stage...");

    match config.data_source.clone() {
        DataSource::Generated => {
            read_generated_data(config, output_tx, metrics);
        }
        DataSource::CsvFile(file_path) => {
            read_csv_data(file_path, config, output_tx, metrics);
        }
        DataSource::JsonFile(file_path) => {
            read_json_data(file_path, config, output_tx, metrics);
        }
        DataSource::TextFile(file_path) => {
            read_text_data(file_path, config, output_tx, metrics);
        }
    }

    println!("✅ Data Reader stage completed");
}

/// Read generated synthetic data
fn read_generated_data(
    config: PipelineConfig,
    output_tx: mpsc::Sender<RawData>,
    metrics: Arc<PipelineMetrics>,
) {
    let mut generator = RecordGenerator::new();
    let start_time = Instant::now();

    // No artificial delay - let the pipeline run at maximum speed
    let _target_delay = Duration::from_nanos(0); // No throttling (available for rate limiting)

    for i in 0..config.input_size {
        let record_start = Instant::now();

        // Generate dynamic user record
        let user_record = generator.generate_record();

        // Convert UserRecord to JSON string for pipeline processing
        let content = format!(
            "{{\"id\":{},\"name\":\"{}\",\"email\":\"{}\",\"age\":{},\"country\":\"{}\",\"subscription_type\":\"{}\",\"last_login\":\"{}\",\"credits\":{}}}",
            user_record.id,
            user_record.name,
            user_record.email,
            user_record.age,
            user_record.country,
            user_record.subscription_type,
            user_record.last_login,
            user_record.credits
        );

        let raw_data = RawData {
            id: i as u64,
            content,
            timestamp: record_start.elapsed().as_millis() as u64,
        };

        // Send data to next stage
        if let Err(_) = output_tx.send(raw_data) {
            println!("❌ Reader: Output channel disconnected");
            metrics.reader.increment_errors();
            return;
        }

        metrics.reader.increment_processed();
        metrics.reader.add_processing_time(record_start.elapsed());

        // No artificial throttling - run at maximum speed
        // (removed sleep for performance testing)

        // Periodic progress reporting
        if i % (config.input_size / 10).max(1) == 0 {
            let current_throughput = (i + 1) as f64 / start_time.elapsed().as_secs_f64();
            println!(
                "📖 Reader: Processed {}/{} items ({:.0} records/sec)",
                i + 1,
                config.input_size,
                current_throughput
            );
        }
    }

    let total_elapsed = start_time.elapsed();
    let final_throughput = config.input_size as f64 / total_elapsed.as_secs_f64();
    println!(
        "📊 Reader: Generated {} records in {:.2}s ({:.0} records/sec)",
        config.input_size,
        total_elapsed.as_secs_f64(),
        final_throughput
    );

    // Close the channel to signal completion
    drop(output_tx);
}

/// Read data from CSV file
fn read_csv_data(
    file_path: String,
    config: PipelineConfig,
    output_tx: mpsc::Sender<RawData>,
    metrics: Arc<PipelineMetrics>,
) {
    println!("📖 Reading CSV file: {}", file_path);

    let file = match File::open(&file_path) {
        Ok(f) => f,
        Err(e) => {
            println!("❌ Failed to open CSV file {}: {}", file_path, e);
            metrics.reader.increment_errors();
            return;
        }
    };

    let mut reader = csv::Reader::from_reader(file);
    let mut count = 0;

    for result in reader.records() {
        let start_time = Instant::now();

        let record = match result {
            Ok(r) => r,
            Err(e) => {
                println!("❌ CSV parsing error: {}", e);
                metrics.reader.increment_errors();
                continue;
            }
        };

        // Convert CSV record to raw data
        let content = record.iter().collect::<Vec<_>>().join(",");
        let raw_data = RawData {
            id: count,
            content,
            timestamp: start_time.elapsed().as_millis() as u64,
        };

        // Send data to next stage
        if let Err(_) = output_tx.send(raw_data) {
            println!("❌ Reader: Output channel disconnected");
            metrics.reader.increment_errors();
            return;
        }

        count += 1;
        metrics.reader.increment_processed();
        metrics.reader.add_processing_time(start_time.elapsed());

        // Simulate processing delay
        if config.processing_delay_ms > 0 {
            coroutine::sleep(Duration::from_millis(config.processing_delay_ms));
        }

        // Periodic progress reporting
        if count % 10 == 0 {
            println!("📖 Reader: Processed {} CSV records", count);
        }
    }

    println!("📖 Reader: Completed reading {} CSV records", count);
    drop(output_tx);
}

/// Read data from JSON file
fn read_json_data(
    file_path: String,
    config: PipelineConfig,
    output_tx: mpsc::Sender<RawData>,
    metrics: Arc<PipelineMetrics>,
) {
    println!("📖 Reading JSON file: {}", file_path);

    let file = match File::open(&file_path) {
        Ok(f) => f,
        Err(e) => {
            println!("❌ Failed to open JSON file {}: {}", file_path, e);
            metrics.reader.increment_errors();
            return;
        }
    };

    let reader = BufReader::new(file);
    let json_value: serde_json::Value = match serde_json::from_reader(reader) {
        Ok(v) => v,
        Err(e) => {
            println!("❌ JSON parsing error: {}", e);
            metrics.reader.increment_errors();
            return;
        }
    };

    let mut count = 0;

    // Handle both single objects and arrays
    let items: Vec<&serde_json::Value> = if json_value.is_array() {
        json_value.as_array().unwrap().iter().collect()
    } else {
        // Single object, wrap in array
        vec![&json_value]
    };

    for item in items {
        let start_time = Instant::now();

        let content = item.to_string();
        let raw_data = RawData {
            id: count,
            content,
            timestamp: start_time.elapsed().as_millis() as u64,
        };

        // Send data to next stage
        if let Err(_) = output_tx.send(raw_data) {
            println!("❌ Reader: Output channel disconnected");
            metrics.reader.increment_errors();
            return;
        }

        count += 1;
        metrics.reader.increment_processed();
        metrics.reader.add_processing_time(start_time.elapsed());

        // Simulate processing delay
        if config.processing_delay_ms > 0 {
            coroutine::sleep(Duration::from_millis(config.processing_delay_ms));
        }

        // Periodic progress reporting
        if count % 10 == 0 {
            println!("📖 Reader: Processed {} JSON records", count);
        }
    }

    println!("📖 Reader: Completed reading {} JSON records", count);
    drop(output_tx);
}

/// Read data from text file
fn read_text_data(
    file_path: String,
    config: PipelineConfig,
    output_tx: mpsc::Sender<RawData>,
    metrics: Arc<PipelineMetrics>,
) {
    println!("📖 Reading text file: {}", file_path);

    let file = match File::open(&file_path) {
        Ok(f) => f,
        Err(e) => {
            println!("❌ Failed to open text file {}: {}", file_path, e);
            metrics.reader.increment_errors();
            return;
        }
    };

    let reader = BufReader::new(file);
    let mut count = 0;

    for line in reader.lines() {
        let start_time = Instant::now();

        let content = match line {
            Ok(l) => l,
            Err(e) => {
                println!("❌ Text reading error: {}", e);
                metrics.reader.increment_errors();
                continue;
            }
        };

        // Skip empty lines
        if content.trim().is_empty() {
            continue;
        }

        let raw_data = RawData {
            id: count,
            content,
            timestamp: start_time.elapsed().as_millis() as u64,
        };

        // Send data to next stage
        if let Err(_) = output_tx.send(raw_data) {
            println!("❌ Reader: Output channel disconnected");
            metrics.reader.increment_errors();
            return;
        }

        count += 1;
        metrics.reader.increment_processed();
        metrics.reader.add_processing_time(start_time.elapsed());

        // Simulate processing delay
        if config.processing_delay_ms > 0 {
            coroutine::sleep(Duration::from_millis(config.processing_delay_ms));
        }

        // Periodic progress reporting
        if count % 10 == 0 {
            println!("📖 Reader: Processed {} text lines", count);
        }
    }

    println!("📖 Reader: Completed reading {} text lines", count);
    drop(output_tx);
}

/// Parser Stage - Converts raw data to structured format
fn parser_stage(
    config: PipelineConfig,
    input_rx: mpsc::Receiver<RawData>,
    output_tx: mpsc::Sender<ParsedData>,
    metrics: Arc<PipelineMetrics>,
) {
    println!("🔄 Starting Parser stage...");

    while let Ok(raw_data) = input_rx.recv() {
        let start_time = Instant::now();

        // Simulate parsing work
        if config.processing_delay_ms > 0 {
            coroutine::sleep(Duration::from_millis(config.processing_delay_ms));
        }

        // Simulate parsing errors
        if config.enable_errors && rand::random::<f64>() < config.error_rate {
            println!("⚠️  Parser: Error processing item {}", raw_data.id);
            metrics.parser.increment_errors();
            continue;
        }

        // Parse the raw data into structured format
        let mut fields = HashMap::new();
        fields.insert("original_content".to_string(), raw_data.content);
        fields.insert("processing_stage".to_string(), "parsed".to_string());
        fields.insert("item_type".to_string(), "data_item".to_string());

        let parsed_data = ParsedData {
            id: raw_data.id,
            fields,
            timestamp: raw_data.timestamp,
        };

        // Send to next stage
        if let Err(_) = output_tx.send(parsed_data) {
            println!("❌ Parser: Output channel disconnected");
            metrics.parser.increment_errors();
            return;
        }

        metrics.parser.increment_processed();
        metrics.parser.add_processing_time(start_time.elapsed());
    }

    drop(output_tx);
    println!("✅ Parser stage completed");
}

/// Transformer Stage - Applies business logic transformations
fn transformer_stage(
    config: PipelineConfig,
    input_rx: mpsc::Receiver<ParsedData>,
    output_tx: mpsc::Sender<TransformedData>,
    metrics: Arc<PipelineMetrics>,
) {
    println!("🔄 Starting Transformer stage...");

    while let Ok(parsed_data) = input_rx.recv() {
        let start_time = Instant::now();

        // Simulate transformation work (optimized for performance testing)
        // (removed sleep for maximum throughput testing)

        // Simulate transformation errors
        if config.enable_errors && rand::random::<f64>() < config.error_rate {
            println!("⚠️  Transformer: Error processing item {}", parsed_data.id);
            metrics.transformer.increment_errors();
            continue;
        }

        // Apply business logic transformations
        let mut processed_fields = HashMap::new();
        for (key, value) in parsed_data.fields.iter() {
            processed_fields.insert(
                format!("transformed_{}", key),
                format!("processed_{}", value),
            );
        }
        processed_fields.insert(
            "transformation_time".to_string(),
            start_time.elapsed().as_millis().to_string(),
        );

        // Calculate a score based on processing time and data characteristics
        let score =
            (parsed_data.id as f64 * 0.1) + (start_time.elapsed().as_millis() as f64 * 0.001);

        let transformed_data = TransformedData {
            id: parsed_data.id,
            processed_fields,
            score,
            timestamp: parsed_data.timestamp,
        };

        // Send to next stage
        if let Err(_) = output_tx.send(transformed_data) {
            println!("❌ Transformer: Output channel disconnected");
            metrics.transformer.increment_errors();
            return;
        }

        metrics.transformer.increment_processed();
        metrics
            .transformer
            .add_processing_time(start_time.elapsed());
    }

    drop(output_tx);
    println!("✅ Transformer stage completed");
}

/// Validator Stage - Validates processed data
fn validator_stage(
    config: PipelineConfig,
    input_rx: mpsc::Receiver<TransformedData>,
    output_tx: mpsc::Sender<ValidatedData>,
    metrics: Arc<PipelineMetrics>,
) {
    println!("🔄 Starting Validator stage...");

    while let Ok(transformed_data) = input_rx.recv() {
        let start_time = Instant::now();

        // Simulate validation work (optimized for performance testing)
        // (removed sleep for maximum throughput testing)

        // Simulate validation errors
        if config.enable_errors && rand::random::<f64>() < config.error_rate {
            println!(
                "⚠️  Validator: Validation failed for item {}",
                transformed_data.id
            );
            metrics.validator.increment_errors();
            continue;
        }

        // Validate the transformed data
        let validation_status = if transformed_data.score > 50.0 {
            "high_quality"
        } else if transformed_data.score > 10.0 {
            "medium_quality"
        } else {
            "low_quality"
        };

        let mut final_data = transformed_data.processed_fields.clone();
        final_data.insert(
            "validation_status".to_string(),
            validation_status.to_string(),
        );
        final_data.insert(
            "validation_time".to_string(),
            start_time.elapsed().as_millis().to_string(),
        );

        let validated_data = ValidatedData {
            id: transformed_data.id,
            final_data,
            score: transformed_data.score,
            timestamp: transformed_data.timestamp,
            validation_status: validation_status.to_string(),
        };

        // Send to next stage
        if let Err(_) = output_tx.send(validated_data) {
            println!("❌ Validator: Output channel disconnected");
            metrics.validator.increment_errors();
            return;
        }

        metrics.validator.increment_processed();
        metrics.validator.add_processing_time(start_time.elapsed());
    }

    drop(output_tx);
    println!("✅ Validator stage completed");
}

/// Writer Stage - Outputs final processed data
fn writer_stage(
    config: PipelineConfig,
    input_rx: mpsc::Receiver<ValidatedData>,
    metrics: Arc<PipelineMetrics>,
) {
    println!("🔄 Starting Writer stage...");
    let mut output_count = 0;

    while let Ok(validated_data) = input_rx.recv() {
        let start_time = Instant::now();

        // Simulate writing work (optimized for performance testing)
        // (removed sleep for maximum throughput testing)

        // Simulate writing errors
        if config.enable_errors && rand::random::<f64>() < config.error_rate {
            println!("⚠️  Writer: Error writing item {}", validated_data.id);
            metrics.writer.increment_errors();
            continue;
        }

        // Write the final data (simulate by printing summary)
        output_count += 1;
        if output_count % (config.input_size / 10).max(1) == 0 {
            println!(
                "💾 Writer: Processed item {} - Score: {:.2} - Status: {}",
                validated_data.id, validated_data.score, validated_data.validation_status
            );
        }

        metrics.writer.increment_processed();
        metrics.writer.add_processing_time(start_time.elapsed());
    }

    println!(
        "✅ Writer stage completed - Total items written: {}",
        output_count
    );
}

/// Parse command line arguments
fn parse_args() -> PipelineConfig {
    let args: Vec<String> = std::env::args().collect();
    let mut config = PipelineConfig::default();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--input-size" => {
                if i + 1 < args.len() {
                    config.input_size = args[i + 1].parse().unwrap_or(config.input_size);
                    i += 1;
                }
            }
            "--delay" => {
                if i + 1 < args.len() {
                    config.processing_delay_ms =
                        args[i + 1].parse().unwrap_or(config.processing_delay_ms);
                    i += 1;
                }
            }
            "--enable-errors" => {
                config.enable_errors = true;
            }
            "--error-rate" => {
                if i + 1 < args.len() {
                    config.error_rate = args[i + 1].parse().unwrap_or(config.error_rate);
                    i += 1;
                }
            }
            "--input-file" => {
                if i + 1 < args.len() {
                    let file_path = args[i + 1].clone();
                    config.input_file = Some(file_path.clone());

                    // Determine data source based on file extension
                    config.data_source = if file_path.ends_with(".csv") {
                        DataSource::CsvFile(file_path)
                    } else if file_path.ends_with(".json") {
                        DataSource::JsonFile(file_path)
                    } else {
                        DataSource::TextFile(file_path)
                    };
                    i += 1;
                }
            }
            "--help" => {
                println!("Pipeline Data Processing Example");
                println!("Usage: cargo run --example pipeline_data_processing [OPTIONS]");
                println!("Options:");
                println!("  --input-size <N>     Number of items to process [default: 1000]");
                println!("  --delay <MS>         Processing delay per item in ms [default: 1]");
                println!("  --enable-errors      Enable random errors in processing");
                println!("  --error-rate <RATE>  Error rate (0.0-1.0) [default: 0.05]");
                println!("  --input-file <FILE>  Input file to process (CSV, JSON, or text)");
                println!("  --help               Show this help message");
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

    println!("🚀 Starting Pipeline Data Processing Example");
    println!("Configuration: {:?}", config);

    // Configure May runtime
    may::config().set_workers(num_cpus::get());

    let start_time = Instant::now();
    let metrics = Arc::new(PipelineMetrics::new());

    // Run the pipeline within a coroutine scope
    may::coroutine::scope(|scope| {
        // Create channels between pipeline stages
        let (reader_tx, reader_rx) = mpsc::channel();
        let (parser_tx, parser_rx) = mpsc::channel();
        let (transformer_tx, transformer_rx) = mpsc::channel();
        let (validator_tx, validator_rx) = mpsc::channel();

        // Spawn pipeline stages as coroutines
        let config_clone = config.clone();
        let metrics_clone = metrics.clone();
        go!(scope, move || {
            data_reader_stage(config_clone, reader_tx, metrics_clone);
        });

        let config_clone = config.clone();
        let metrics_clone = metrics.clone();
        go!(scope, move || {
            parser_stage(config_clone, reader_rx, parser_tx, metrics_clone);
        });

        let config_clone = config.clone();
        let metrics_clone = metrics.clone();
        go!(scope, move || {
            transformer_stage(config_clone, parser_rx, transformer_tx, metrics_clone);
        });

        let config_clone = config.clone();
        let metrics_clone = metrics.clone();
        go!(scope, move || {
            validator_stage(config_clone, transformer_rx, validator_tx, metrics_clone);
        });

        let config_clone = config.clone();
        let metrics_clone = metrics.clone();
        go!(scope, move || {
            writer_stage(config_clone, validator_rx, metrics_clone);
        });

        // Monitor progress
        let metrics_clone = metrics.clone();
        let config_clone = config.clone();
        go!(scope, move || {
            let mut last_processed = 0;
            let mut stable_count = 0;

            loop {
                coroutine::sleep(Duration::from_millis(500)); // Check more frequently
                let total_processed = metrics_clone.writer.processed.load(Ordering::Relaxed);

                // For file input, we don't know the exact count ahead of time
                // So we detect when processing has stopped
                if total_processed == last_processed {
                    stable_count += 1;
                    if stable_count >= 4 {
                        // Processing has been stable for 2 seconds, likely done
                        break;
                    }
                } else {
                    stable_count = 0;
                    last_processed = total_processed;

                    // Only print progress updates when there's actual progress
                    if matches!(config_clone.data_source, DataSource::Generated) {
                        if total_processed >= config_clone.input_size as u64 {
                            break;
                        }
                        if total_processed % 10 == 0 || total_processed == 1 {
                            println!(
                                "📊 Progress: {}/{} items completed",
                                total_processed, config_clone.input_size
                            );
                        }
                    } else {
                        if total_processed % 5 == 0 || total_processed == 1 {
                            println!("📊 Progress: {} items completed", total_processed);
                        }
                    }
                }
            }
        });
    });

    let total_time = start_time.elapsed();

    // Print final metrics
    metrics.print_summary();
    println!(
        "\n⏱️  Total Processing Time: {:.2}s",
        total_time.as_secs_f64()
    );
    println!(
        "🚀 Throughput: {:.2} items/second",
        config.input_size as f64 / total_time.as_secs_f64()
    );

    println!("\n✨ Pipeline Data Processing Example completed successfully!");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pipeline_config_default() {
        let config = PipelineConfig::default();
        assert_eq!(config.input_size, 1000);
        assert_eq!(config.processing_delay_ms, 1);
        assert!(!config.enable_errors);
        assert_eq!(config.error_rate, 0.05);
    }

    #[test]
    fn test_stage_metrics() {
        let metrics = StageMetrics::default();

        metrics.increment_processed();
        metrics.increment_processed();
        metrics.increment_errors();
        metrics.add_processing_time(Duration::from_millis(100));

        let (processed, errors, time_ms) = metrics.get_stats();
        assert_eq!(processed, 2);
        assert_eq!(errors, 1);
        assert_eq!(time_ms, 100);
    }

    #[test]
    fn test_small_pipeline() {
        may::config().set_workers(2);

        let config = PipelineConfig {
            input_size: 10,
            processing_delay_ms: 0,
            enable_errors: false,
            error_rate: 0.0,
            input_file: None,
            data_source: DataSource::Generated,
        };

        let metrics = Arc::new(PipelineMetrics::new());

        may::coroutine::scope(|scope| {
            let (reader_tx, reader_rx) = mpsc::channel();
            let (parser_tx, parser_rx) = mpsc::channel();
            let (transformer_tx, transformer_rx) = mpsc::channel();
            let (validator_tx, validator_rx) = mpsc::channel();

            let metrics_clone = metrics.clone();
            go!(scope, move || {
                data_reader_stage(config.clone(), reader_tx, metrics_clone);
            });

            let metrics_clone = metrics.clone();
            go!(scope, move || {
                parser_stage(config.clone(), reader_rx, parser_tx, metrics_clone);
            });

            let metrics_clone = metrics.clone();
            go!(scope, move || {
                transformer_stage(config.clone(), parser_rx, transformer_tx, metrics_clone);
            });

            let metrics_clone = metrics.clone();
            go!(scope, move || {
                validator_stage(config.clone(), transformer_rx, validator_tx, metrics_clone);
            });

            let metrics_clone = metrics.clone();
            go!(scope, move || {
                writer_stage(config.clone(), validator_rx, metrics_clone);
            });
        });

        // Verify all items were processed
        let (processed, errors, _) = metrics.writer.get_stats();
        assert_eq!(processed, 10);
        assert_eq!(errors, 0);
    }

    #[test]
    fn test_csv_file_input() {
        use std::fs;
        use std::io::Write;

        may::config().set_workers(2);

        // Create a temporary CSV file
        let temp_file = "test_data.csv";
        let mut file = fs::File::create(temp_file).unwrap();
        writeln!(file, "id,name,value").unwrap();
        writeln!(file, "1,test1,100").unwrap();
        writeln!(file, "2,test2,200").unwrap();

        let config = PipelineConfig {
            input_size: 1000, // This will be ignored for file input
            processing_delay_ms: 0,
            enable_errors: false,
            error_rate: 0.0,
            input_file: Some(temp_file.to_string()),
            data_source: DataSource::CsvFile(temp_file.to_string()),
        };

        let metrics = Arc::new(PipelineMetrics::new());

        may::coroutine::scope(|scope| {
            let (reader_tx, reader_rx) = mpsc::channel();
            let (parser_tx, parser_rx) = mpsc::channel();
            let (transformer_tx, transformer_rx) = mpsc::channel();
            let (validator_tx, validator_rx) = mpsc::channel();

            let metrics_clone = metrics.clone();
            go!(scope, move || {
                data_reader_stage(config.clone(), reader_tx, metrics_clone);
            });

            let metrics_clone = metrics.clone();
            go!(scope, move || {
                parser_stage(config.clone(), reader_rx, parser_tx, metrics_clone);
            });

            let metrics_clone = metrics.clone();
            go!(scope, move || {
                transformer_stage(config.clone(), parser_rx, transformer_tx, metrics_clone);
            });

            let metrics_clone = metrics.clone();
            go!(scope, move || {
                validator_stage(config.clone(), transformer_rx, validator_tx, metrics_clone);
            });

            let metrics_clone = metrics.clone();
            go!(scope, move || {
                writer_stage(config.clone(), validator_rx, metrics_clone);
            });
        });

        // Verify CSV records were processed (header + 2 data rows = 3 total)
        let (processed, errors, _) = metrics.writer.get_stats();
        assert_eq!(processed, 3);
        assert_eq!(errors, 0);

        // Clean up
        let _ = fs::remove_file(temp_file);
    }
}
