#!/usr/bin/env python3
"""
Coverage Formatter for May Coroutine Library
Transforms LLVM coverage text output into beautiful ASCII tables for GitHub CI
"""

import sys
import re
import argparse
from typing import List, Dict, Tuple, Optional
from dataclasses import dataclass
from pathlib import Path


@dataclass
class CoverageStats:
    """Coverage statistics for a file or total"""
    lines_total: int
    lines_covered: int
    lines_percent: float
    functions_total: int
    functions_covered: int
    functions_percent: float
    branches_total: int = 0
    branches_covered: int = 0
    branches_percent: float = 0.0


@dataclass
class FileCoverage:
    """Coverage data for a single file"""
    filepath: str
    stats: CoverageStats


class CoverageParser:
    """Parser for LLVM coverage text output"""
    
    def __init__(self):
        # Regex patterns for parsing LLVM coverage output
        self.file_pattern = re.compile(r'^(/[^:]+):$')
        self.summary_line_pattern = re.compile(
            r'^\s*(\d+)\s+(\d+)\s+([\d.]+)%\s+(\d+)\s+(\d+)\s+([\d.]+)%\s+(\d+)\s+(\d+)\s*(.*)$'
        )
        self.total_pattern = re.compile(r'^TOTAL\s+(\d+)\s+(\d+)\s+([\d.]+)%\s+(\d+).*$')
        
    def parse_coverage_output(self, content: str) -> Tuple[List[FileCoverage], Optional[CoverageStats]]:
        """Parse LLVM coverage text output"""
        lines = content.strip().split('\n')
        files = []
        total_stats = None
        current_file = None
        
        for line in lines:
            line = line.strip()
            
            # Check for file path
            file_match = self.file_pattern.match(line)
            if file_match:
                current_file = file_match.group(1)
                continue
            
            # Check for TOTAL line
            total_match = self.total_pattern.match(line)
            if total_match:
                try:
                    lines_total = int(total_match.group(1))
                    lines_covered = int(total_match.group(2))
                    lines_percent = float(total_match.group(3))
                    functions_total = int(total_match.group(4))
                    
                    # For now, assume functions covered based on percentage
                    # This is a simplified parsing - real data may have more info
                    functions_covered = int(functions_total * lines_percent / 100)
                    functions_percent = lines_percent  # Approximation
                    
                    total_stats = CoverageStats(
                        lines_total=lines_total,
                        lines_covered=lines_covered,
                        lines_percent=lines_percent,
                        functions_total=functions_total,
                        functions_covered=functions_covered,
                        functions_percent=functions_percent
                    )
                except (ValueError, IndexError):
                    pass
                continue
            
            # Check for summary line with stats
            if current_file and self.summary_line_pattern.match(line):
                stats = self._parse_stats_line(line)
                if stats:
                    files.append(FileCoverage(current_file, stats))
                current_file = None
        
        return files, total_stats
    
    def _parse_stats_line(self, line: str) -> Optional[CoverageStats]:
        """Parse a statistics line"""
        # More flexible parsing for different formats
        parts = line.split()
        if len(parts) < 6:
            return None
        
        try:
            lines_total = int(parts[0])
            lines_covered = int(parts[1])
            lines_percent = float(parts[2].rstrip('%'))
            functions_total = int(parts[3])
            functions_covered = int(parts[4])
            functions_percent = float(parts[5].rstrip('%'))
            
            # Optional branch coverage
            branches_total = 0
            branches_covered = 0
            branches_percent = 0.0
            
            if len(parts) >= 9:
                branches_total = int(parts[6])
                branches_covered = int(parts[7])
                if parts[8] != '-':
                    branches_percent = float(parts[8].rstrip('%'))
            
            return CoverageStats(
                lines_total=lines_total,
                lines_covered=lines_covered,
                lines_percent=lines_percent,
                functions_total=functions_total,
                functions_covered=functions_covered,
                functions_percent=functions_percent,
                branches_total=branches_total,
                branches_covered=branches_covered,
                branches_percent=branches_percent
            )
        except (ValueError, IndexError):
            return None


class TableFormatter:
    """Formats coverage data into beautiful ASCII tables"""
    
    def __init__(self, style: str = "github"):
        self.style = style
        
    def format_coverage_table(self, files: List[FileCoverage], total_stats: Optional[CoverageStats], 
                            max_files: int = 20) -> str:
        """Format coverage data into a beautiful ASCII table"""
        
        if self.style == "github":
            return self._format_github_table(files, total_stats, max_files)
        elif self.style == "ascii":
            return self._format_ascii_table(files, total_stats, max_files)
        else:
            return self._format_simple_table(files, total_stats, max_files)
    
    def _format_github_table(self, files: List[FileCoverage], total_stats: Optional[CoverageStats], 
                           max_files: int) -> str:
        """Format as GitHub-flavored markdown table"""
        
        # Sort files by coverage percentage (lowest first for attention)
        sorted_files = sorted(files, key=lambda f: f.stats.lines_percent)
        
        # Take worst performers and best performers
        display_files = []
        if len(sorted_files) > max_files:
            worst = sorted_files[:max_files//2]
            best = sorted_files[-(max_files//2):]
            display_files = worst + [None] + best  # None for separator
        else:
            display_files = sorted_files
        
        output = []
        output.append("## 📊 Coverage Report")
        output.append("")
        
        if total_stats:
            # Summary badges
            line_color = self._get_coverage_color(total_stats.lines_percent)
            func_color = self._get_coverage_color(total_stats.functions_percent)
            
            output.append("### Overall Coverage")
            output.append(f"![Lines](https://img.shields.io/badge/Lines-{total_stats.lines_percent:.1f}%25-{line_color})")
            output.append(f"![Functions](https://img.shields.io/badge/Functions-{total_stats.functions_percent:.1f}%25-{func_color})")
            output.append("")
            
            # Summary table
            output.append("| Metric | Covered | Total | Percentage |")
            output.append("|--------|---------|-------|------------|")
            output.append(f"| **Lines** | {total_stats.lines_covered:,} | {total_stats.lines_total:,} | **{total_stats.lines_percent:.1f}%** |")
            output.append(f"| **Functions** | {total_stats.functions_covered:,} | {total_stats.functions_total:,} | **{total_stats.functions_percent:.1f}%** |")
            if total_stats.branches_total > 0:
                output.append(f"| **Branches** | {total_stats.branches_covered:,} | {total_stats.branches_total:,} | **{total_stats.branches_percent:.1f}%** |")
            output.append("")
        
        # File details table
        if display_files:
            output.append("### File Coverage Details")
            output.append("| File | Lines | Functions | Coverage |")
            output.append("|------|-------|-----------|----------|")
            
            for file_cov in display_files:
                if file_cov is None:
                    output.append("| ... | ... | ... | ... |")
                    continue
                
                filename = Path(file_cov.filepath).name
                if len(filename) > 30:
                    filename = "..." + filename[-27:]
                
                line_badge = self._format_percentage_badge(file_cov.stats.lines_percent)
                func_badge = self._format_percentage_badge(file_cov.stats.functions_percent)
                
                coverage_emoji = self._get_coverage_emoji(file_cov.stats.lines_percent)
                
                output.append(f"| `{filename}` | {file_cov.stats.lines_covered}/{file_cov.stats.lines_total} | {file_cov.stats.functions_covered}/{file_cov.stats.functions_total} | {coverage_emoji} {line_badge} |")
        
        return "\n".join(output)
    
    def _format_ascii_table(self, files: List[FileCoverage], total_stats: Optional[CoverageStats], 
                          max_files: int) -> str:
        """Format as beautiful ASCII table"""
        
        sorted_files = sorted(files, key=lambda f: f.stats.lines_percent)
        display_files = sorted_files[:max_files] if len(sorted_files) > max_files else sorted_files
        
        output = []
        
        # Header
        output.append("┌─" + "─" * 78 + "─┐")
        output.append("│" + " " * 20 + "📊 MAY COROUTINE LIBRARY COVERAGE REPORT" + " " * 18 + "│")
        output.append("├─" + "─" * 78 + "─┤")
        
        if total_stats:
            # Summary section
            output.append(f"│ OVERALL: {total_stats.lines_percent:5.1f}% lines  │  {total_stats.functions_percent:5.1f}% functions  │  {len(files)} files analyzed" + " " * 15 + "│")
            output.append("├─" + "─" * 78 + "─┤")
        
        # Table header
        output.append("│ File" + " " * 36 + "│ Lines" + " " * 7 + "│ Functions │ Coverage │")
        output.append("├─" + "─" * 39 + "─┼─" + "─" * 11 + "─┼─" + "─" * 9 + "─┼─" + "─" * 8 + "─┤")
        
        # File rows
        for file_cov in display_files:
            filename = Path(file_cov.filepath).name
            if len(filename) > 38:
                filename = "..." + filename[-35:]
            
            lines_str = f"{file_cov.stats.lines_covered}/{file_cov.stats.lines_total}"
            funcs_str = f"{file_cov.stats.functions_covered}/{file_cov.stats.functions_total}"
            coverage_str = f"{file_cov.stats.lines_percent:5.1f}%"
            
            # Color coding with emojis
            emoji = self._get_coverage_emoji(file_cov.stats.lines_percent)
            
            output.append(f"│ {filename:<38} │ {lines_str:>11} │ {funcs_str:>9} │ {emoji} {coverage_str:>5} │")
        
        # Footer
        output.append("└─" + "─" * 39 + "─┴─" + "─" * 11 + "─┴─" + "─" * 9 + "─┴─" + "─" * 8 + "─┘")
        
        return "\n".join(output)
    
    def _format_simple_table(self, files: List[FileCoverage], total_stats: Optional[CoverageStats], 
                           max_files: int) -> str:
        """Format as simple ASCII table"""
        
        sorted_files = sorted(files, key=lambda f: f.stats.lines_percent)
        display_files = sorted_files[:max_files] if len(sorted_files) > max_files else sorted_files
        
        output = []
        output.append("May Coroutine Library - Coverage Report")
        output.append("=" * 60)
        
        if total_stats:
            output.append(f"Overall Coverage: {total_stats.lines_percent:.1f}% lines, {total_stats.functions_percent:.1f}% functions")
            output.append("")
        
        # Simple table
        output.append(f"{'File':<30} {'Lines':<12} {'Functions':<12} {'Coverage':<10}")
        output.append("-" * 66)
        
        for file_cov in display_files:
            filename = Path(file_cov.filepath).name
            if len(filename) > 28:
                filename = "..." + filename[-25:]
            
            lines_str = f"{file_cov.stats.lines_covered}/{file_cov.stats.lines_total}"
            funcs_str = f"{file_cov.stats.functions_covered}/{file_cov.stats.functions_total}"
            coverage_str = f"{file_cov.stats.lines_percent:.1f}%"
            
            output.append(f"{filename:<30} {lines_str:<12} {funcs_str:<12} {coverage_str:<10}")
        
        return "\n".join(output)
    
    def _get_coverage_color(self, percentage: float) -> str:
        """Get color for coverage percentage"""
        if percentage >= 90:
            return "brightgreen"
        elif percentage >= 80:
            return "green"
        elif percentage >= 70:
            return "yellowgreen"
        elif percentage >= 60:
            return "yellow"
        elif percentage >= 50:
            return "orange"
        else:
            return "red"
    
    def _get_coverage_emoji(self, percentage: float) -> str:
        """Get emoji for coverage percentage"""
        if percentage >= 95:
            return "🟢"
        elif percentage >= 85:
            return "🟡"
        elif percentage >= 70:
            return "🟠"
        else:
            return "🔴"
    
    def _format_percentage_badge(self, percentage: float) -> str:
        """Format percentage as a badge-like string"""
        return f"**{percentage:.1f}%**"


def main():
    parser = argparse.ArgumentParser(description="Format LLVM coverage output into beautiful ASCII tables")
    parser.add_argument("input", nargs="?", help="Input file (default: stdin)")
    parser.add_argument("-s", "--style", choices=["github", "ascii", "simple"], 
                       default="github", help="Output style (default: github)")
    parser.add_argument("-m", "--max-files", type=int, default=20, 
                       help="Maximum number of files to show (default: 20)")
    parser.add_argument("-o", "--output", help="Output file (default: stdout)")
    
    args = parser.parse_args()
    
    # Read input
    if args.input:
        with open(args.input, 'r') as f:
            content = f.read()
    else:
        content = sys.stdin.read()
    
    # Parse coverage data
    parser_obj = CoverageParser()
    files, total_stats = parser_obj.parse_coverage_output(content)
    
    if not files and not total_stats:
        print("Error: No coverage data found in input", file=sys.stderr)
        sys.exit(1)
    
    # Format output
    formatter = TableFormatter(args.style)
    formatted_output = formatter.format_coverage_table(files, total_stats, args.max_files)
    
    # Write output
    if args.output:
        with open(args.output, 'w') as f:
            f.write(formatted_output)
        print(f"Coverage report written to {args.output}")
    else:
        print(formatted_output)


if __name__ == "__main__":
    main() 