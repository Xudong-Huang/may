#!/usr/bin/env python3
"""
Test Reporter for May Coroutine Library
Parses test output and creates beautiful reports showing individual test results
"""

import sys
import re
import argparse
import json
from typing import List, Dict, Optional, Tuple
from dataclasses import dataclass, field
from pathlib import Path
from datetime import datetime


@dataclass
class TestResult:
    """Individual test result"""
    name: str
    status: str  # ok, FAILED, ignored, etc.
    duration: Optional[float] = None
    module: str = ""
    details: str = ""
    error_message: str = ""
    panic_message: str = ""


@dataclass
class TestSuite:
    """Test suite results"""
    name: str
    tests: List[TestResult] = field(default_factory=list)
    total: int = 0
    passed: int = 0
    failed: int = 0
    ignored: int = 0
    filtered: int = 0
    duration: Optional[float] = None


@dataclass
class TestReport:
    """Complete test report"""
    suites: List[TestSuite] = field(default_factory=list)
    total_tests: int = 0
    total_passed: int = 0
    total_failed: int = 0
    total_ignored: int = 0
    total_filtered: int = 0
    total_duration: Optional[float] = None
    timestamp: str = field(default_factory=lambda: datetime.now().isoformat())


class TestParser:
    """Parser for Rust test output"""
    
    def __init__(self):
        # Regex patterns for parsing test output
        self.test_line_pattern = re.compile(
            r'^test\s+([^.]+(?:\.[^.]+)*)::\s*([^.]+(?:\.[^.]+)*)\s+\.\.\.\s+(\w+)(?:\s+<([^>]+)>)?'
        )
        self.summary_pattern = re.compile(
            r'^test result:\s+(\w+)\.\s+(\d+)\s+passed;\s+(\d+)\s+failed;\s+(\d+)\s+ignored;\s+(\d+)\s+measured;\s+(\d+)\s+filtered out(?:;\s+finished in\s+([\d.]+)s)?'
        )
        self.running_pattern = re.compile(r'^running\s+(\d+)\s+tests?')
        self.panic_pattern = re.compile(r'thread\s+\'([^\']+)\'\s+panicked at\s+([^:]+):(\d+):(\d+):')
        self.error_pattern = re.compile(r'Error:\s+(.+)')
        self.suite_pattern = re.compile(r'Running\s+(.+)\s+\((.+)\)')
        
    def parse_test_output(self, content: str) -> TestReport:
        """Parse test output and return structured report"""
        lines = content.strip().split('\n')
        report = TestReport()
        current_suite = None
        current_test = None
        in_panic = False
        panic_buffer = []
        
        for line in lines:
            line = line.strip()
            
            # Check for suite start
            suite_match = self.suite_pattern.match(line)
            if suite_match:
                if current_suite:
                    report.suites.append(current_suite)
                current_suite = TestSuite(name=suite_match.group(1))
                continue
            
            # Check for running X tests
            running_match = self.running_pattern.match(line)
            if running_match and current_suite:
                current_suite.total = int(running_match.group(1))
                continue
            
            # Check for individual test results
            test_match = self.test_line_pattern.match(line)
            if test_match:
                module = test_match.group(1)
                test_name = test_match.group(2)
                status = test_match.group(3)
                details = test_match.group(4) or ""
                
                test_result = TestResult(
                    name=test_name,
                    status=status,
                    module=module,
                    details=details
                )
                
                if current_suite:
                    current_suite.tests.append(test_result)
                    if status == "ok":
                        current_suite.passed += 1
                    elif status == "FAILED":
                        current_suite.failed += 1
                    elif status == "ignored":
                        current_suite.ignored += 1
                
                current_test = test_result
                continue
            
            # Check for panic messages
            panic_match = self.panic_pattern.match(line)
            if panic_match:
                in_panic = True
                panic_buffer = [line]
                if current_test:
                    current_test.panic_message = line
                continue
            
            # Collect panic details
            if in_panic:
                if line.startswith("note:") or line.startswith("stack backtrace:") or line == "":
                    in_panic = False
                    if current_test:
                        current_test.error_message = "\n".join(panic_buffer)
                    panic_buffer = []
                else:
                    panic_buffer.append(line)
                continue
            
            # Check for test summary
            summary_match = self.summary_pattern.match(line)
            if summary_match and current_suite:
                current_suite.passed = int(summary_match.group(2))
                current_suite.failed = int(summary_match.group(3))
                current_suite.ignored = int(summary_match.group(4))
                current_suite.filtered = int(summary_match.group(6))
                if summary_match.group(7):
                    current_suite.duration = float(summary_match.group(7))
                continue
        
        # Add final suite
        if current_suite:
            report.suites.append(current_suite)
        
        # Calculate totals
        for suite in report.suites:
            report.total_tests += suite.total
            report.total_passed += suite.passed
            report.total_failed += suite.failed
            report.total_ignored += suite.ignored
            report.total_filtered += suite.filtered
            if suite.duration:
                if report.total_duration is None:
                    report.total_duration = 0
                report.total_duration += suite.duration
        
        return report


class TestReportFormatter:
    """Formats test reports into various output styles"""
    
    def __init__(self, style: str = "github"):
        self.style = style
    
    def format_test_report(self, report: TestReport, show_passed: bool = False, 
                          show_details: bool = True) -> str:
        """Format test report into specified style"""
        
        if self.style == "github":
            return self._format_github_report(report, show_passed, show_details)
        elif self.style == "ascii":
            return self._format_ascii_report(report, show_passed, show_details)
        elif self.style == "json":
            return self._format_json_report(report)
        else:
            return self._format_simple_report(report, show_passed, show_details)
    
    def _format_github_report(self, report: TestReport, show_passed: bool, 
                             show_details: bool) -> str:
        """Format as GitHub-flavored markdown"""
        
        output = []
        
        # Header
        output.append("# 🧪 Test Report - May Coroutine Library")
        output.append("")
        
        # Summary badges
        if report.total_failed > 0:
            status_badge = "![Status](https://img.shields.io/badge/Status-FAILED-red)"
            status_emoji = "❌"
        else:
            status_badge = "![Status](https://img.shields.io/badge/Status-PASSED-brightgreen)"
            status_emoji = "✅"
        
        pass_rate = (report.total_passed / report.total_tests * 100) if report.total_tests > 0 else 0
        pass_color = "brightgreen" if pass_rate >= 95 else "green" if pass_rate >= 85 else "yellow"
        
        output.append("## 📊 Summary")
        output.append(status_badge)
        output.append(f"![Tests](https://img.shields.io/badge/Tests-{report.total_tests}-blue)")
        output.append(f"![Passed](https://img.shields.io/badge/Passed-{report.total_passed}-brightgreen)")
        if report.total_failed > 0:
            output.append(f"![Failed](https://img.shields.io/badge/Failed-{report.total_failed}-red)")
        if report.total_ignored > 0:
            output.append(f"![Ignored](https://img.shields.io/badge/Ignored-{report.total_ignored}-yellow)")
        output.append(f"![Pass Rate](https://img.shields.io/badge/Pass%20Rate-{pass_rate:.1f}%25-{pass_color})")
        output.append("")
        
        # Summary table
        output.append("| Metric | Count | Percentage |")
        output.append("|--------|-------|------------|")
        output.append(f"| {status_emoji} **Total Tests** | {report.total_tests:,} | 100.0% |")
        output.append(f"| ✅ **Passed** | {report.total_passed:,} | {pass_rate:.1f}% |")
        if report.total_failed > 0:
            fail_rate = (report.total_failed / report.total_tests * 100) if report.total_tests > 0 else 0
            output.append(f"| ❌ **Failed** | {report.total_failed:,} | {fail_rate:.1f}% |")
        if report.total_ignored > 0:
            ignore_rate = (report.total_ignored / report.total_tests * 100) if report.total_tests > 0 else 0
            output.append(f"| ⏭️ **Ignored** | {report.total_ignored:,} | {ignore_rate:.1f}% |")
        if report.total_duration:
            output.append(f"| ⏱️ **Duration** | {report.total_duration:.2f}s | - |")
        output.append("")
        
        # Failed tests section
        if report.total_failed > 0:
            output.append("## ❌ Failed Tests")
            output.append("")
            
            for suite in report.suites:
                failed_tests = [t for t in suite.tests if t.status == "FAILED"]
                if failed_tests:
                    output.append(f"### {suite.name}")
                    output.append("")
                    
                    for test in failed_tests:
                        output.append(f"#### 🔴 `{test.module}::{test.name}`")
                        if test.details:
                            output.append(f"**Details:** {test.details}")
                        if test.panic_message:
                            output.append("**Error:**")
                            output.append("```")
                            output.append(test.panic_message)
                            if test.error_message:
                                output.append(test.error_message)
                            output.append("```")
                        output.append("")
        
        # Test suites breakdown
        if len(report.suites) > 1:
            output.append("## 📦 Test Suites")
            output.append("")
            output.append("| Suite | Total | Passed | Failed | Ignored | Pass Rate |")
            output.append("|-------|-------|--------|--------|---------|-----------|")
            
            for suite in report.suites:
                suite_pass_rate = (suite.passed / suite.total * 100) if suite.total > 0 else 0
                status_icon = "✅" if suite.failed == 0 else "❌"
                output.append(f"| {status_icon} `{suite.name}` | {suite.total} | {suite.passed} | {suite.failed} | {suite.ignored} | {suite_pass_rate:.1f}% |")
            output.append("")
        
        # Passed tests (if requested)
        if show_passed and report.total_passed > 0:
            output.append("## ✅ Passed Tests")
            output.append("")
            
            for suite in report.suites:
                passed_tests = [t for t in suite.tests if t.status == "ok"]
                if passed_tests:
                    output.append(f"### {suite.name}")
                    output.append("")
                    
                    # Group by module
                    modules = {}
                    for test in passed_tests:
                        if test.module not in modules:
                            modules[test.module] = []
                        modules[test.module].append(test)
                    
                    for module, tests in modules.items():
                        output.append(f"#### `{module}`")
                        for test in tests:
                            output.append(f"- ✅ `{test.name}`")
                        output.append("")
        
        return "\n".join(output)
    
    def _format_ascii_report(self, report: TestReport, show_passed: bool, 
                            show_details: bool) -> str:
        """Format as ASCII art report"""
        
        output = []
        
        # Header
        output.append("┌─" + "─" * 78 + "─┐")
        output.append("│" + " " * 25 + "🧪 MAY COROUTINE TEST REPORT" + " " * 25 + "│")
        output.append("├─" + "─" * 78 + "─┤")
        
        # Summary
        status = "PASSED" if report.total_failed == 0 else "FAILED"
        status_color = "✅" if report.total_failed == 0 else "❌"
        pass_rate = (report.total_passed / report.total_tests * 100) if report.total_tests > 0 else 0
        
        output.append(f"│ STATUS: {status_color} {status:<8} │ TESTS: {report.total_tests:>4} │ PASSED: {report.total_passed:>4} │ FAILED: {report.total_failed:>4} │")
        if report.total_duration:
            output.append(f"│ DURATION: {report.total_duration:>6.2f}s │ PASS RATE: {pass_rate:>5.1f}% │ IGNORED: {report.total_ignored:>4} │")
        output.append("├─" + "─" * 78 + "─┤")
        
        # Failed tests
        if report.total_failed > 0:
            output.append("│" + " " * 30 + "❌ FAILED TESTS" + " " * 33 + "│")
            output.append("├─" + "─" * 78 + "─┤")
            
            for suite in report.suites:
                failed_tests = [t for t in suite.tests if t.status == "FAILED"]
                if failed_tests:
                    suite_name = suite.name if len(suite.name) <= 76 else suite.name[:73] + "..."
                    output.append(f"│ 📦 {suite_name:<75} │")
                    output.append("├─" + "─" * 78 + "─┤")
                    
                    for test in failed_tests:
                        test_name = f"{test.module}::{test.name}"
                        if len(test_name) > 72:
                            test_name = test_name[:69] + "..."
                        output.append(f"│ 🔴 {test_name:<74} │")
                        if test.details:
                            details = test.details if len(test.details) <= 72 else test.details[:69] + "..."
                            output.append(f"│    {details:<74} │")
                    output.append("├─" + "─" * 78 + "─┤")
        
        # Suite summary
        if len(report.suites) > 1:
            output.append("│" + " " * 30 + "📦 TEST SUITES" + " " * 34 + "│")
            output.append("├─" + "─" * 78 + "─┤")
            output.append("│ Suite" + " " * 35 + "│ Total │ Pass │ Fail │ Rate │")
            output.append("├─" + "─" * 39 + "─┼─" + "─" * 5 + "─┼─" + "─" * 4 + "─┼─" + "─" * 4 + "─┼─" + "─" * 4 + "─┤")
            
            for suite in report.suites:
                suite_name = suite.name if len(suite.name) <= 38 else suite.name[:35] + "..."
                suite_pass_rate = (suite.passed / suite.total * 100) if suite.total > 0 else 0
                status_icon = "✅" if suite.failed == 0 else "❌"
                output.append(f"│ {status_icon} {suite_name:<36} │ {suite.total:>5} │ {suite.passed:>4} │ {suite.failed:>4} │ {suite_pass_rate:>3.0f}% │")
        
        # Footer
        output.append("└─" + "─" * 78 + "─┘")
        
        return "\n".join(output)
    
    def _format_json_report(self, report: TestReport) -> str:
        """Format as JSON"""
        
        def convert_to_dict(obj):
            if hasattr(obj, '__dict__'):
                return {k: convert_to_dict(v) for k, v in obj.__dict__.items()}
            elif isinstance(obj, list):
                return [convert_to_dict(item) for item in obj]
            else:
                return obj
        
        return json.dumps(convert_to_dict(report), indent=2)
    
    def _format_simple_report(self, report: TestReport, show_passed: bool, 
                             show_details: bool) -> str:
        """Format as simple text report"""
        
        output = []
        
        # Header
        output.append("May Coroutine Library - Test Report")
        output.append("=" * 50)
        
        # Summary
        status = "PASSED" if report.total_failed == 0 else "FAILED"
        pass_rate = (report.total_passed / report.total_tests * 100) if report.total_tests > 0 else 0
        
        output.append(f"Status: {status}")
        output.append(f"Total Tests: {report.total_tests}")
        output.append(f"Passed: {report.total_passed}")
        output.append(f"Failed: {report.total_failed}")
        output.append(f"Ignored: {report.total_ignored}")
        output.append(f"Pass Rate: {pass_rate:.1f}%")
        if report.total_duration:
            output.append(f"Duration: {report.total_duration:.2f}s")
        output.append("")
        
        # Failed tests
        if report.total_failed > 0:
            output.append("FAILED TESTS:")
            output.append("-" * 20)
            
            for suite in report.suites:
                failed_tests = [t for t in suite.tests if t.status == "FAILED"]
                if failed_tests:
                    output.append(f"\n{suite.name}:")
                    for test in failed_tests:
                        output.append(f"  FAIL: {test.module}::{test.name}")
                        if test.details:
                            output.append(f"        {test.details}")
        
        return "\n".join(output)


def main():
    parser = argparse.ArgumentParser(description="Format Rust test output into beautiful reports")
    parser.add_argument("input", nargs="?", help="Input file (default: stdin)")
    parser.add_argument("-s", "--style", choices=["github", "ascii", "simple", "json"], 
                       default="github", help="Output style (default: github)")
    parser.add_argument("-o", "--output", help="Output file (default: stdout)")
    parser.add_argument("--show-passed", action="store_true", 
                       help="Show passed tests in addition to failed ones")
    parser.add_argument("--show-details", action="store_true", default=True,
                       help="Show detailed error messages")
    
    args = parser.parse_args()
    
    # Read input
    if args.input:
        with open(args.input, 'r') as f:
            content = f.read()
    else:
        content = sys.stdin.read()
    
    # Parse test data
    parser_obj = TestParser()
    report = parser_obj.parse_test_output(content)
    
    if report.total_tests == 0:
        print("Error: No test data found in input", file=sys.stderr)
        sys.exit(1)
    
    # Format output
    formatter = TestReportFormatter(args.style)
    formatted_output = formatter.format_test_report(
        report, 
        show_passed=args.show_passed, 
        show_details=args.show_details
    )
    
    # Write output
    if args.output:
        with open(args.output, 'w') as f:
            f.write(formatted_output)
        print(f"Test report written to {args.output}")
    else:
        print(formatted_output)


if __name__ == "__main__":
    main() 