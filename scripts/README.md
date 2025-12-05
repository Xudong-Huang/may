# Coverage Tools for May Coroutine Library

This directory contains tools for generating beautiful coverage reports for the May coroutine library, optimized for GitHub CI/CD pipelines.

## 📊 Available Tools

### 1. Coverage Script (`coverage.sh`)
Main coverage collection script with multiple output formats.

```bash
# Basic usage
./scripts/coverage.sh                    # Default summary table
./scripts/coverage.sh -f formatted       # Beautiful GitHub-style table
./scripts/coverage.sh -f formatted --style ascii  # ASCII art table

# Advanced usage
./scripts/coverage.sh -f lcov -o coverage.lcov     # LCOV format
./scripts/coverage.sh -f json -o coverage.json     # JSON format
./scripts/coverage.sh -f text -o coverage.txt      # Detailed line-by-line
```

**Options:**
- `-f, --format`: Output format (`summary`, `formatted`, `text`, `lcov`, `json`)
- `-o, --output`: Output file (default: stdout)
- `--style`: Table style for formatted output (`github`, `ascii`, `simple`)
- `--stack-size`: Stack size for coroutines (default: 8MB)

### 2. Coverage Formatter (`coverage_formatter.py`)
Python tool that transforms LLVM coverage text output into beautiful ASCII tables.

```bash
# Direct usage
cargo llvm-cov --text | python3 scripts/coverage_formatter.py --style github
python3 scripts/coverage_formatter.py input.txt --style ascii
python3 scripts/coverage_formatter.py --style simple -o report.md
```

**Styles:**
- `github`: GitHub-flavored markdown with badges and emojis
- `ascii`: Beautiful ASCII art table with Unicode box drawing
- `simple`: Plain text table for basic terminals

## 🎨 Output Examples

### GitHub Style (`--style github`)
```markdown
## 📊 Coverage Report

### Overall Coverage
![Lines](https://img.shields.io/badge/Lines-78.5%25-yellowgreen)
![Functions](https://img.shields.io/badge/Functions-78.5%25-yellowgreen)

| Metric | Covered | Total | Percentage |
|--------|---------|-------|------------|
| **Lines** | 3,266 | 15,190 | **78.5%** |
| **Functions** | 1,036 | 1,320 | **78.5%** |

### File Coverage Details
| File | Lines | Functions | Coverage |
|------|-------|-----------|----------|
| `config.rs` | 318/318 | 25/25 | 🔴 **0.0%** |
| `mutex.rs` | 67/650 | 4/53 | 🟡 **89.7%** |
| `coroutine.rs` | 420/450 | 32/35 | 🟡 **93.3%** |
| `mpsc.rs` | 22/1288 | 2/121 | 🟢 **98.3%** |
```

### ASCII Art Style (`--style ascii`)
```
┌────────────────────────────────────────────────────────────────────────────────┐
│                    📊 MAY COROUTINE LIBRARY COVERAGE REPORT                  │
├────────────────────────────────────────────────────────────────────────────────┤
│ OVERALL:  78.5% lines  │   78.5% functions  │  6 files analyzed               │
├────────────────────────────────────────────────────────────────────────────────┤
│ File                                    │ Lines       │ Functions │ Coverage │
├─────────────────────────────────────────┼─────────────┼───────────┼──────────┤
│ config.rs                              │     318/318 │     25/25 │ 🔴   0.0% │
│ mutex.rs                               │      67/650 │      4/53 │ 🟡  89.7% │
│ coroutine.rs                           │     420/450 │     32/35 │ 🟡  93.3% │
│ mpsc.rs                                │     22/1288 │     2/121 │ 🟢  98.3% │
└─────────────────────────────────────────┴─────────────┴───────────┴──────────┘
```

## 🚀 GitHub CI Integration

### GitHub Actions Example
```yaml
name: Coverage Report

on: [push, pull_request]

jobs:
  coverage:
    runs-on: ubuntu-latest
    steps:
    - uses: actions/checkout@v4
    
    - name: Install Rust
      uses: dtolnay/rust-toolchain@nightly
      with:
        components: llvm-tools-preview
    
    - name: Install coverage tools
      run: |
        cargo install cargo-llvm-cov
        pip install --upgrade pip
    
    - name: Generate coverage report
      run: |
        ./scripts/coverage.sh -f formatted --style github > coverage_report.md
    
    - name: Comment PR with coverage
      uses: actions/github-script@v6
      if: github.event_name == 'pull_request'
      with:
        script: |
          const fs = require('fs');
          const coverage = fs.readFileSync('coverage_report.md', 'utf8');
          github.rest.issues.createComment({
            issue_number: context.issue.number,
            owner: context.repo.owner,
            repo: context.repo.repo,
            body: coverage
          });
```

### Simple CI Script
```bash
#!/bin/bash
# ci_coverage.sh - Simple coverage for CI
set -e

echo "🔍 Running May coroutine library coverage analysis..."
./scripts/coverage.sh -f formatted --style github

# Exit with error if coverage is below threshold
COVERAGE=$(./scripts/coverage.sh | grep "OVERALL" | grep -o "[0-9.]*%" | head -1 | sed 's/%//')
if (( $(echo "$COVERAGE < 75.0" | bc -l) )); then
    echo "❌ Coverage $COVERAGE% is below 75% threshold"
    exit 1
fi

echo "✅ Coverage $COVERAGE% meets requirements"
```

## 🔧 Nextest Integration

The tools work seamlessly with nextest for enhanced test execution:

```bash
# Run tests with nextest and generate coverage
cargo nextest run --all-features
./scripts/coverage.sh -f formatted --style ascii

# Coverage-specific nextest profile
cargo nextest run --profile coverage
./scripts/coverage.sh -f formatted --style github
```

## 🎯 Features

### Coverage Formatter Features
- **Smart file filtering**: Shows worst and best performers
- **Color-coded coverage**: 🔴 Red (<70%), 🟡 Yellow (70-95%), 🟢 Green (95%+)
- **GitHub badges**: Automatic shield.io badge generation
- **Responsive tables**: Adapts to different file counts
- **Multiple formats**: Markdown, ASCII art, plain text

### Coverage Script Features
- **Coroutine-optimized**: Increased stack size for stability
- **Example filtering**: Excludes examples from coverage
- **Multiple outputs**: Summary, detailed, LCOV, JSON formats
- **Error handling**: Graceful failure with helpful messages
- **Configurable**: Stack size, output format, file limits

## 🐛 Troubleshooting

### Common Issues

1. **Stack overflow in tests**
   ```bash
   # Increase stack size
   ./scripts/coverage.sh --stack-size 16777216  # 16MB
   ```

2. **Missing llvm-tools**
   ```bash
   rustup component add llvm-tools-preview
   ```

3. **Python dependencies**
   ```bash
   pip install dataclasses  # For Python < 3.7
   ```

### Debug Mode
```bash
# Enable verbose output
RUST_BACKTRACE=1 ./scripts/coverage.sh -f formatted --style ascii
```

## 📈 Integration with External Tools

### Codecov
```bash
./scripts/coverage.sh -f lcov -o coverage.lcov
curl -s https://codecov.io/bash | bash -s -- -f coverage.lcov
```

### Coveralls
```bash
./scripts/coverage.sh -f lcov -o coverage.lcov
coveralls --lcov-file coverage.lcov
```

### SonarQube
```bash
./scripts/coverage.sh -f lcov -o coverage.lcov
# Configure sonar-project.properties with:
# sonar.rust.lcov.reportPaths=coverage.lcov
```

---

*These tools are specifically designed for the May coroutine library's unique testing requirements, including coroutine stack management and timing-sensitive test execution.* 