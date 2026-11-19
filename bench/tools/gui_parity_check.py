#!/usr/bin/env python3
"""gui_parity_check.py - Functional & visual parity audit suite for fast-routing GUI (fr-gui)
versus upstream Java Freerouting 2.3.0 GUI.

Reads operational evidence from bench/results/gui-parity/results.json.
"""

from __future__ import annotations

import argparse
import json
import os
import sys

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.dirname(os.path.dirname(SCRIPT_DIR))
DEFAULT_RESULTS_JSON = os.path.join(REPO_ROOT, "bench", "results", "gui-parity", "results.json")

def load_results(path: str = DEFAULT_RESULTS_JSON) -> dict:
    if not os.path.isfile(path):
        print(f"Error: results file {path} not found.", file=sys.stderr)
        sys.exit(1)
    with open(path, "r", encoding="utf-8") as f:
        return json.load(f)

def generate_report(data: dict) -> str:
    lines = []
    lines.append("# GUI Parity Scorecard: fr-gui (Rust GPUI) vs Java Freerouting 2.3.0\n")

    meta = data.get("audit_metadata", {})
    lines.append("## 1. Audit Metadata & Environment Status")
    lines.append(f"- **Target**: {meta.get('target', 'fr-gui')}")
    lines.append(f"- **Commit**: `{meta.get('git_commit', 'HEAD')}`")
    lines.append(f"- **Date**: {meta.get('date', '2026-09-10')}")
    lines.append(f"- **Display Status**: `{meta.get('display_state')}`")
    lines.append(f"- **Display Diagnostic**: {meta.get('display_diagnostic')}\n")

    win_cap = data.get("window_capture_verification", {})
    live_cap = win_cap.get("live_window_screencapture", {})
    off_cap = win_cap.get("offline_export_renders", {})

    lines.append("## 2. Window Capture & Screenshot Protocol")
    lines.append(f"- **Live Window Screencapture**: **{live_cap.get('status_display')}**")
    lines.append(f"  - Command: `{live_cap.get('command')}`")
    lines.append(f"  - Error: `{live_cap.get('error')}`")
    lines.append(f"  - Mechanism: {live_cap.get('mechanism')}")
    lines.append(f"- **Offline Export Visual Renders**: **{off_cap.get('status_display')}**")
    lines.append(f"  - Title Bug Fixed: `{off_cap.get('title_bug_fixed')}`")
    for c in off_cap.get("captures", []):
        lines.append(f"  - `{c}`")
    lines.append("")

    must_items = data.get("must_features", [])
    should_items = data.get("should_features", [])
    must_pass = sum(1 for f in must_items if f.get("status") == "PASS")
    should_pass = sum(1 for f in should_items if f.get("status") == "PASS")

    lines.append("## 3. Executive Coverage Summary")
    lines.append(f"- **Must-Have Coverage**: {must_pass} / {len(must_items)} ({must_pass / len(must_items) * 100:.1f}%)")
    lines.append(f"- **Should-Have Coverage**: {should_pass} / {len(should_items)} ({should_pass / len(should_items) * 100:.1f}%)\n")

    lines.append("## 4. Must-Have Feature Operational Verification\n")
    lines.append("| ID | Feature | Status | Verification Target | Concrete Operational Evidence |")
    lines.append("|---|---|---|---|---|")
    for f in must_items:
        test_t = f.get("test_target", "-")
        ev = f.get("evidence", "-")
        lines.append(f"| `{f['id']}` | **{f['title']}** | **{f['status']}** | `{test_t}` | {ev} |")
    lines.append("")

    lines.append("## 5. Should-Have Feature Audit & Gap Analysis\n")
    lines.append("| ID | Feature | Status | Verification / Remediation |")
    lines.append("|---|---|---|---|")
    for f in should_items:
        status = f.get("status")
        if status == "PASS":
            detail = f"Verified: {f.get('evidence', '')}"
        else:
            detail = f"Gap: {f.get('remediation', '')}"
        lines.append(f"| `{f['id']}` | **{f['title']}** | **{status}** | {detail} |")
    lines.append("")

    perf = data.get("performance", {})
    lines.append("## 6. Performance Benchmarks")
    lines.append(f"- **15k Items Snapshot (`cla45-compute-hdi6cs-stage2`)**: **{perf.get('snapshot_15k_items_ms')} ms** (budget: < {perf.get('snapshot_budget_ms')} ms, verified by `{perf.get('snapshot_test')}`)")
    lines.append(f"- **15k Items Rasterization + Export**: **{perf.get('raster_export_15k_items_sec')} s**")
    lines.append(f"- **Memory RSS (`cla45-sensor-l6`)**: `fr-gui` **{perf.get('memory_rss_sensor_l6_mb')} MB** vs Java GUI **{perf.get('memory_rss_java_l6_mb')} MB** (**{perf.get('memory_reduction_factor')}× lower**)")

    return "\n".join(lines)

def main():
    parser = argparse.ArgumentParser(description="Audit GUI Parity vs Java Freerouting")
    parser.add_argument("--report", action="store_true", help="Print Markdown scorecard report")
    parser.add_argument("--json", action="store_true", help="Output raw JSON data")
    parser.add_argument("--file", default=DEFAULT_RESULTS_JSON, help="Path to results.json")
    args = parser.parse_args()

    data = load_results(args.file)
    if args.json:
        print(json.dumps(data, indent=2))
    else:
        print(generate_report(data))

if __name__ == "__main__":
    main()
