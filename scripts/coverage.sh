#!/bin/bash
# Coverage reporting script for May coroutine library
# Provides ASCII table output perfect for CI/CD pipelines

set -e

# Default values
FORMAT="summary"
OUTPUT=""
STACK_SIZE="8388608"  # 8MB stack for coroutine stability
STYLE="github"        # Table style for formatter
SHOW_PASSED="false"   # Show passed tests in test report
TESTS_ONLY="false"    # Generate only test report

# Parse command line arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        -f|--format)
            FORMAT="$2"
            shift 2
            ;;
        -o|--output)
            OUTPUT="$2"
            shift 2
            ;;
        --stack-size)
            STACK_SIZE="$2"
            shift 2
            ;;
        --style)
            STYLE="$2"
            shift 2
            ;;
        --show-passed)
            SHOW_PASSED="true"
            shift
            ;;
        --tests-only)
            TESTS_ONLY="true"
            shift
            ;;
        -h|--help)
            echo "Coverage reporting for May coroutine library"
            echo ""
            echo "Usage: $0 [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  -f, --format FORMAT     Output format: summary, text, lcov, json, formatted, tests, pr-report"
            echo "  -o, --output FILE       Output file (default: stdout)"
            echo "  --stack-size SIZE       Stack size for coroutines (default: 8388608)"
            echo "  --style STYLE           Table style: github, ascii, simple (default: github)"
            echo "  --show-passed           Show passed tests in test report"
            echo "  --tests-only            Generate only test report (no coverage)"
            echo "  -h, --help             Show this help message"
            echo ""
            echo "Examples:"
            echo "  $0                      # Summary table to stdout"
            echo "  $0 -f formatted         # Beautiful formatted table (GitHub style)"
            echo "  $0 -f formatted --style ascii  # ASCII art table"
            echo "  $0 -f tests             # Detailed test report"
            echo "  $0 -f tests --show-passed  # Test report with passed tests"
            echo "  $0 -f pr-report        # GitHub PR report (tests + coverage)"
            echo "  $0 -f ci-report        # CI-optimized PR report (fast)"
            echo "  $0 -f text             # Detailed line-by-line coverage"
            echo "  $0 -f lcov -o cov.lcov # LCOV format for external tools"
            echo "  $0 -f json -o cov.json # JSON format for parsing"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            exit 1
            ;;
    esac
done

# Set environment variables
export RUST_MIN_STACK="$STACK_SIZE"

# Build the command
CMD="cargo llvm-cov --lib --ignore-filename-regex 'examples/'"

case $FORMAT in
    summary)
        # Default summary table format
        ;;
    formatted)
        # Use Python formatter for beautiful tables
        CMD="$CMD 2>/dev/null | python3 scripts/coverage_formatter.py --style $STYLE"
        if [[ -n "$OUTPUT" ]]; then
            CMD="$CMD --output $OUTPUT"
        fi
        ;;
    tests)
        # Generate detailed test report
        TEST_CMD="RUST_MIN_STACK=$STACK_SIZE cargo test --lib --all-features"
        if [[ "$SHOW_PASSED" == "true" ]]; then
            TEST_CMD="$TEST_CMD 2>&1 | python3 scripts/test_reporter.py --style $STYLE --show-passed"
        else
            TEST_CMD="$TEST_CMD 2>&1 | python3 scripts/test_reporter.py --style $STYLE"
        fi
        if [[ -n "$OUTPUT" ]]; then
            TEST_CMD="$TEST_CMD --output $OUTPUT"
        fi
        
        if [[ "$TESTS_ONLY" == "true" ]]; then
            # Only run tests, no coverage
            CMD="$TEST_CMD"
        else
            # Run both tests and coverage
            echo "🔍 Running tests and coverage analysis..."
            echo "📊 Test format: $FORMAT, Style: $STYLE"
            echo "⚡ Stack size: $STACK_SIZE bytes"
            echo ""
            
            # Run test report first
            echo "📋 Generating test report..."
            eval "$TEST_CMD"
            echo ""
            
            # Then run coverage
            echo "📊 Generating coverage report..."
            # Don't override CMD, let it fall through to coverage
        fi
        ;;
    pr-report)
        # Generate comprehensive GitHub PR report
        echo "🔍 Generating comprehensive GitHub PR report..."
        echo "📊 This will run tests and coverage analysis"
        echo ""
        
        # Get script directory
        SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
        
        # Run the dedicated PR report script
        if "$SCRIPT_DIR/generate_pr_report.sh"; then
            echo "✅ GitHub PR report generated: PR_TEST_COVERAGE_REPORT.md"
            exit 0
        else
            echo "❌ PR report generation failed"
            exit 1
                 fi
         ;;
    ci-report)
        # Generate CI-optimized GitHub PR report
        echo "🔍 Generating CI-optimized GitHub PR report..."
        echo "📊 This is the fast version for CI/CD pipelines"
        echo ""
        
        # Get script directory
        SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
        
        # Run the CI-optimized PR report script
        if "$SCRIPT_DIR/ci_pr_report.sh"; then
            echo "✅ CI PR report generated: PR_TEST_COVERAGE_REPORT.md"
            exit 0
        else
            echo "❌ CI PR report generation failed"
            exit 1
        fi
        ;;
    text)
        CMD="$CMD --text"
        if [[ -n "$OUTPUT" ]]; then
            CMD="$CMD --output-path $OUTPUT"
        fi
        ;;
    lcov)
        CMD="$CMD --lcov"
        if [[ -n "$OUTPUT" ]]; then
            CMD="$CMD --output-path $OUTPUT"
        fi
        ;;
    json)
        CMD="$CMD --json"
        if [[ -n "$OUTPUT" ]]; then
            CMD="$CMD --output-path $OUTPUT"
        fi
        ;;
    *)
        echo "Error: Unknown format '$FORMAT'"
        echo "Supported formats: summary, text, lcov, json, formatted, tests, pr-report, ci-report"
        exit 1
        ;;
esac

# Run the coverage command
echo "🔍 Running coverage analysis with format: $FORMAT"
echo "📊 Stack size: ${STACK_SIZE} bytes"
echo "⚡ Command: $CMD"
echo ""

if [[ -n "$OUTPUT" ]]; then
    eval "$CMD"
    echo "✅ Coverage report saved to: $OUTPUT"
else
    eval "$CMD"
fi 