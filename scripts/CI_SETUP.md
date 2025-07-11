# CI Setup for May Coroutine Library

This document describes the CI setup for automated test and coverage reporting in GitHub Pull Requests.

## 🚀 Quick Start

### Local Testing
```bash
# Generate CI-optimized report (fast)
./scripts/ci_pr_report.sh

# Or use the coverage script wrapper
./scripts/coverage.sh -f ci-report
```

### GitHub Actions Integration
The CI workflow is automatically triggered on:
- Pull requests to `main` or `develop` branches
- Changes to source code, tests, or build files
- Manual workflow dispatch

## 📁 Files Overview

### Core Scripts
- **`scripts/ci_pr_report.sh`** - CI-optimized report generator (fast, reliable)
- **`scripts/coverage.sh`** - Unified coverage script with multiple formats
- **`.github/workflows/pr-report.yml`** - GitHub Actions workflow

### Generated Reports
- **`PR_TEST_COVERAGE_REPORT.md`** - Comprehensive PR report with badges and metrics

## 🔧 CI Workflow Features

### Automated PR Comments
- **Smart Updates**: Updates existing comments instead of creating new ones
- **Rich Formatting**: Professional badges, tables, and metrics
- **Failure Handling**: Clear indication of test failures with actionable information
- **Artifact Storage**: Reports saved as downloadable artifacts (30-day retention)

### Performance Optimized
- **Fast Execution**: ~2-3 seconds for full test + coverage analysis
- **Caching**: Rust dependencies cached between runs
- **Parallel Processing**: Optimized for GitHub Actions runners

### Error Handling
- **Graceful Failures**: Continues with partial reports if tests fail
- **Clear Diagnostics**: Detailed error messages for debugging
- **Exit Codes**: Proper exit codes for CI pipeline integration

## 📊 Report Format

The generated report includes:

### Summary Section (Top)
- Status badges (PASSED/FAILED)
- Test counts and pass rates
- Execution duration
- Quick metrics table

### Test Results
- Failed tests (if any) with clear indicators
- Test execution summary
- Module and category breakdown

### Coverage Analysis
- Line and function coverage percentages
- Coverage breakdown table
- Professional badges for metrics

### Final Summary (Bottom)
- Key metrics and highlights
- Quality assurance checklist
- Coverage highlights by module
- Action items (if failures detected)

## 🛠️ Configuration

### Environment Variables
```bash
RUST_MIN_STACK=8388608  # 8MB stack for coroutine stability
CARGO_TERM_COLOR=always # Colored output
RUST_BACKTRACE=1       # Enhanced error reporting
```

### Workflow Triggers
```yaml
on:
  pull_request:
    branches: [ main, develop ]
    paths:
      - 'src/**'
      - 'may_queue/**'
      - 'tests/**'
      - 'Cargo.toml'
      - 'Cargo.lock'
```

## 🔍 Troubleshooting

### Common Issues

#### Script Permissions
```bash
chmod +x scripts/ci_pr_report.sh
chmod +x scripts/coverage.sh
```

#### Missing Dependencies
```bash
# Install cargo-llvm-cov
cargo install cargo-llvm-cov

# Install Rust nightly (for coverage)
rustup toolchain install nightly
rustup component add llvm-tools-preview
```

#### Test Failures
The CI script handles test failures gracefully:
- Continues with report generation
- Shows failed tests in the report
- Sets appropriate exit codes for CI

### Debug Mode
```bash
# Run with debug output
bash -x ./scripts/ci_pr_report.sh

# Check individual components
cargo test --lib --all-features
cargo llvm-cov --lib --ignore-filename-regex 'examples/'
```

## 📈 Metrics and Benchmarks

### Typical Performance
- **Test Execution**: ~1.8 seconds
- **Coverage Analysis**: ~3-4 seconds
- **Report Generation**: <1 second
- **Total CI Time**: ~30-45 seconds (including setup)

### Coverage Targets
- **Line Coverage**: Target 80%+ (currently ~79%)
- **Function Coverage**: Target 80%+ (currently ~79%)
- **Critical Paths**: 95%+ coverage for safety mechanisms

## 🚀 Future Enhancements

### Planned Features
- **Trend Analysis**: Coverage trends over time
- **Performance Benchmarks**: Execution time tracking
- **Module-Specific Reports**: Detailed per-module breakdowns
- **Integration Tests**: Extended test coverage beyond unit tests

### External Integrations
- **Codecov**: Upload coverage data for trend analysis
- **SonarQube**: Code quality metrics
- **Dependabot**: Automated dependency updates

## 💡 Best Practices

### For Contributors
1. **Run locally first**: Test the CI script before pushing
2. **Check coverage**: Aim for >80% coverage on new code
3. **Fix failures**: Address test failures before requesting review
4. **Review reports**: Check the generated PR report for completeness

### For Maintainers
1. **Monitor trends**: Watch for coverage regressions
2. **Update thresholds**: Adjust coverage targets as codebase matures
3. **Optimize CI**: Keep CI execution time under 60 seconds
4. **Review workflows**: Regularly update GitHub Actions versions

---

*This CI system is designed to provide comprehensive, fast, and reliable test and coverage reporting for the May coroutine library development workflow.* 