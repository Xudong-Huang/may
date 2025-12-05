# Sample Data Files

This directory contains sample data files used for testing and demonstrating the advanced coroutine examples in the May library.

## Files Overview

### 1. `users.csv`
**Format**: CSV (Comma-Separated Values)  
**Records**: 20 user records  
**Use Cases**: 
- Pipeline data processing example
- User data transformation and validation
- CSV parsing and processing demonstrations

**Schema**:
- `id`: Unique user identifier
- `name`: Full name
- `email`: Email address
- `age`: Age in years
- `country`: Country of residence
- `signup_date`: Account creation date
- `subscription_type`: Account type (basic/premium)
- `last_login`: Last login date

### 2. `transactions.json`
**Format**: JSON (JavaScript Object Notation)  
**Records**: 20 transaction records  
**Use Cases**:
- Fan-out/fan-in processing example
- Financial transaction processing
- JSON parsing and analysis

**Schema**:
- `id`: Unique transaction identifier
- `user_id`: Associated user ID
- `amount`: Transaction amount
- `currency`: Currency code
- `merchant`: Merchant name
- `category`: Transaction category
- `timestamp`: Transaction timestamp
- `status`: Transaction status
- `payment_method`: Payment method used
- `description`: Transaction description

### 3. `events.txt`
**Format**: Plain text log format  
**Records**: 48 log entries  
**Use Cases**:
- Producer-consumer pattern example
- Log processing and analysis
- Event stream processing

**Schema**: Each line contains:
- Timestamp (ISO 8601 format)
- Log level (INFO, DEBUG, WARN, ERROR)
- Component/module name
- Log message with structured data

### 4. `sensor_data.xml`
**Format**: XML (eXtensible Markup Language)  
**Records**: 12 sensor readings  
**Use Cases**:
- Pipeline processing with XML parsing
- IoT sensor data processing
- Structured data transformation

**Schema**:
- `id`: Sensor identifier
- `timestamp`: Reading timestamp
- `location`: Physical location (building, floor, room)
- `measurements`: Sensor values (temperature, humidity, pressure, light, CO2)
- `status`: Alert status (normal, warning, critical)

## Usage in Examples

### Pipeline Data Processing
```bash
# Process user data from CSV
cargo run --example pipeline_data_processing -- --input-file examples/sample_data/users.csv

# Process sensor data from XML
cargo run --example pipeline_data_processing -- --input-file examples/sample_data/sensor_data.xml
```

### Fan-Out/Fan-In Processing
```bash
# Process transaction data
cargo run --example fan_out_fan_in -- --input-file examples/sample_data/transactions.json

# Process with multiple workers
cargo run --example fan_out_fan_in -- --input-file examples/sample_data/transactions.json --workers 4
```

### Producer-Consumer Pattern
```bash
# Process log events
cargo run --example producer_consumer_bounded -- --input-file examples/sample_data/events.txt

# Process with custom producer/consumer ratios
cargo run --example producer_consumer_bounded -- --input-file examples/sample_data/events.txt --producers 2 --consumers 3
```

## Data Characteristics

### Volume
- **Small**: 12-48 records per file
- **Purpose**: Suitable for quick testing and demonstration
- **Scalability**: Examples can be easily extended to handle larger datasets

### Diversity
- **Multiple formats**: CSV, JSON, XML, plain text
- **Different domains**: Users, transactions, logs, sensor data
- **Varied complexity**: Simple flat records to nested structures

### Realism
- **Realistic data**: Names, addresses, transactions, log entries
- **Proper formatting**: Valid timestamps, currencies, email addresses
- **Edge cases**: Different statuses, error conditions, warnings

## Extending Sample Data

To add more sample data:

1. **Create new files** in this directory
2. **Follow naming conventions**: `{domain}_{format}.{ext}`
3. **Update this README** with new file descriptions
4. **Add usage examples** in the respective example files

### Guidelines for New Sample Data

- **Keep files small** (< 1MB) for quick testing
- **Use realistic data** that represents real-world scenarios
- **Include edge cases** like errors, warnings, or unusual values
- **Document the schema** clearly in this README
- **Provide usage examples** for each new file

## Testing with Sample Data

All sample data files are designed to work with the existing examples without modification. They provide:

- **Consistent results** for reproducible testing
- **Varied scenarios** to test different code paths
- **Performance benchmarks** for comparing implementations
- **Educational value** for understanding data processing patterns

## Data Privacy

All sample data is **synthetic** and does not contain any real personal information. The data is generated for testing purposes only and is safe to use in development and demonstration environments. 