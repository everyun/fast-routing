#!/usr/bin/env python3
"""Compare a gate.json against a baseline gate.json board by board.

Determines WORSE / same / better per board, checks added violations and
regressions in unrouted connections, verifies doors/model consistency,
and exits with code 0 on acceptable result, non-zero on regression.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple


def find_latest_baseline(repo_root: Path) -> Path:
    """Find gate.json in the latest commit under bench/results/main-sweeps/."""
    sweeps_dir = repo_root / "bench" / "results" / "main-sweeps"
    if not sweeps_dir.is_dir():
        raise FileNotFoundError(f"Baseline directory not found: {sweeps_dir}")

    # Inspect all candidate directories containing main/gate.json
    candidates = []
    for entry in sweeps_dir.iterdir():
        if entry.is_dir():
            gate_path = entry / "main" / "gate.json"
            if gate_path.is_file():
                # Query commit timestamp if directory name looks like a git commit hash
                commit_ts = None
                commit_hash = entry.name
                try:
                    import subprocess
                    res = subprocess.run(
                        ["git", "show", "-s", "--format=%ct", commit_hash],
                        cwd=str(repo_root),
                        capture_output=True,
                        text=True,
                    )
                    if res.returncode == 0 and res.stdout.strip().isdigit():
                        commit_ts = int(res.stdout.strip())
                except Exception:
                    pass

                # Fallback to st_mtime if git commit timestamp is not available
                ts = commit_ts if commit_ts is not None else gate_path.stat().st_mtime
                candidates.append((ts, entry.name, gate_path))

    if not candidates:
        raise FileNotFoundError(f"No baseline main/gate.json found under {sweeps_dir}")

    candidates.sort(key=lambda x: x[0], reverse=True)
    return candidates[0][2]


def load_gate(path: Path) -> Tuple[List[Dict[str, Any]], Dict[str, Any]]:
    """Load gate.json and return (list of records, map fixture -> record)."""
    with open(path, "r", encoding="utf-8") as f:
        data = json.load(f)
    mapping = {r["fixture"]: r for r in data if "fixture" in r}
    return data, mapping


def classify_board(
    base: Optional[Dict[str, Any]],
    ours: Optional[Dict[str, Any]],
    allow_deadline_noise: int = 0,
) -> Dict[str, Any]:
    """Compare a single fixture between base and ours.

    Returns a dict with:
      fixture, base_u, base_v, ours_u, ours_v, du, dv, added, verdict, note
    """
    if ours is None:
        return {
            "verdict": "MISSING",
            "base_u": None if base is None else base.get("ours_unrouted"),
            "base_v": None if base is None else base.get("ours_violations"),
            "ours_u": None,
            "ours_v": None,
            "du": 0,
            "dv": 0,
            "added": 0,
            "note": "ours missing",
        }

    fixture = ours.get("fixture", "")
    ou = ours.get("ours_unrouted")
    ov = ours.get("ours_violations")
    java_class = ours.get("java_class") or (base.get("java_class") if base else "")

    if ou is None:
        # Not routed or error
        return {
            "fixture": fixture,
            "base_u": None if base is None else base.get("ours_unrouted"),
            "base_v": None if base is None else base.get("ours_violations"),
            "ours_u": None,
            "ours_v": None,
            "du": 0,
            "dv": 0,
            "added": 0,
            "verdict": "ERROR",
            "note": "no ours routing result",
        }

    if base is None or base.get("ours_unrouted") is None:
        # No baseline to compare against
        return {
            "fixture": fixture,
            "base_u": None,
            "base_v": None,
            "ours_u": ou,
            "ours_v": ov,
            "du": 0,
            "dv": 0,
            "added": 0,
            "verdict": "NEW",
            "note": "no baseline",
        }

    bu = base.get("ours_unrouted")
    bv = base.get("ours_violations")

    du = ou - bu
    dv = ov - bv
    added = max(0, dv)

    # Check timeout board noise tolerance
    is_timeout = java_class == "java_timeout"
    effective_worse_u = du > 0
    note_parts = []

    if is_timeout and allow_deadline_noise > 0:
        if 0 < du <= allow_deadline_noise:
            effective_worse_u = False
            note_parts.append(f"deadline noise +{du}u <= {allow_deadline_noise} tolerated")

    if added > 0:
        verdict = "WORSE"
        note_parts.append(f"+{added} added violations")
    elif effective_worse_u:
        verdict = "WORSE"
        note_parts.append(f"+{du} unrouted")
    elif du < 0 or dv < 0:
        verdict = "BETTER"
        if du < 0:
            note_parts.append(f"{du} unrouted")
        if dv < 0:
            note_parts.append(f"{dv} violations")
    else:
        verdict = "SAME"

    return {
        "fixture": fixture,
        "base_u": bu,
        "base_v": bv,
        "ours_u": ou,
        "ours_v": ov,
        "du": du,
        "dv": dv,
        "added": added,
        "verdict": verdict,
        "note": "; ".join(note_parts),
    }


def compare_gates(
    ours_path: Path,
    base_path: Path,
    allow_deadline_noise: int = 0,
) -> Tuple[List[Dict[str, Any]], Dict[str, Any]]:
    """Compare two gate.json files."""
    ours_list, ours_map = load_gate(ours_path)
    base_list, base_map = load_gate(base_path)

    # Collect metadata / modes
    ours_modes = {r.get("doors_mode") for r in ours_list if r.get("doors_mode")}
    base_modes = {r.get("doors_mode") for r in base_list if r.get("doors_mode")}

    # Union of fixtures routed in ours or base
    fixtures = sorted(
        set(
            k
            for k, v in ours_map.items()
            if v.get("ours_unrouted") is not None
        )
        | set(
            k
            for k, v in base_map.items()
            if v.get("ours_unrouted") is not None
        )
    )

    rows = []
    worse_count = 0
    better_count = 0
    same_count = 0
    total_added_v = 0
    total_regressed_u = 0

    for f in fixtures:
        row = classify_board(
            base_map.get(f),
            ours_map.get(f),
            allow_deadline_noise=allow_deadline_noise,
        )
        rows.append(row)
        if row["verdict"] == "WORSE":
            worse_count += 1
            if row["added"] > 0:
                total_added_v += row["added"]
            if row["du"] > 0:
                total_regressed_u += row["du"]
        elif row["verdict"] == "BETTER":
            better_count += 1
        elif row["verdict"] == "SAME":
            same_count += 1

    summary = {
        "ours_path": str(ours_path),
        "base_path": str(base_path),
        "ours_modes": sorted(ours_modes),
        "base_modes": sorted(base_modes),
        "mode_mismatch": bool(ours_modes and base_modes and ours_modes != base_modes),
        "total_boards": len(rows),
        "worse_count": worse_count,
        "better_count": better_count,
        "same_count": same_count,
        "total_added_v": total_added_v,
        "total_regressed_u": total_regressed_u,
        "allow_deadline_noise": allow_deadline_noise,
        "can_merge": (worse_count == 0),
    }

    return rows, summary


def print_comparison_table(
    rows: List[Dict[str, Any]],
    summary: Dict[str, Any],
    stream=sys.stdout,
) -> None:
    """Print ASCII formatted comparison table."""
    ours_mode_str = ", ".join(summary["ours_modes"]) or "unknown"
    base_mode_str = ", ".join(summary["base_modes"]) or "unknown"

    print("=" * 95, file=stream)
    print(f"ACCEPTANCE QUALITY COMPARISON", file=stream)
    print(f"  Ours:     {summary['ours_path']} (doors_mode={ours_mode_str})", file=stream)
    print(f"  Baseline: {summary['base_path']} (doors_mode={base_mode_str})", file=stream)

    if summary["mode_mismatch"]:
        print("\n" + "!" * 95, file=stream)
        print(f"WARNING: DOORS MODE MISMATCH DETECTED!", file=stream)
        print(f"  Ours mode:     {ours_mode_str}", file=stream)
        print(f"  Baseline mode: {base_mode_str}", file=stream)
        print(f"Comparing different router modes (e.g. full vs default) may cause severe quality divergence.", file=stream)
        print("!" * 95 + "\n", file=stream)

    if summary["allow_deadline_noise"] > 0:
        print(f"  Note: --allow-deadline-noise={summary['allow_deadline_noise']} is active for timeout boards.", file=stream)
    print("=" * 95, file=stream)

    hdr = f"{'Fixture':<42} | {'Baseline':<10} | {'Ours':<10} | {'Δu':>4} | {'Added':>5} | {'Verdict':<7} | {'Notes'}"
    print(hdr, file=stream)
    print("-" * 95, file=stream)

    for r in rows:
        bu = r["base_u"]
        bv = r["base_v"]
        ou = r["ours_u"]
        ov = r["ours_v"]

        base_str = "n/a" if bu is None else f"{bu}u/{bv}v"
        ours_str = "n/a" if ou is None else f"{ou}u/{ov}v"
        du_str = f"{r['du']:+d}" if ou is not None and bu is not None else "-"
        add_str = f"{r['added']:+d}" if r['added'] > 0 else "0"

        v_display = r["verdict"]
        print(f"{r['fixture']:<42} | {base_str:<10} | {ours_str:<10} | {du_str:>4} | {add_str:>5} | {v_display:<7} | {r['note']}", file=stream)

    print("-" * 95, file=stream)
    print(f"Summary: {summary['total_boards']} boards | "
          f"Better: {summary['better_count']} | "
          f"Same: {summary['same_count']} | "
          f"Worse (regressed): {summary['worse_count']}", file=stream)
    if summary["total_added_v"] > 0:
        print(f"Added Violations: {summary['total_added_v']} (FAIL)", file=stream)

    if summary["can_merge"]:
        print("\n===> ACCEPTANCE VERDICT: PASS (can merge)", file=stream)
    else:
        print(f"\n===> ACCEPTANCE VERDICT: FAIL ({summary['worse_count']} worse boards; cannot merge)", file=stream)
    print("=" * 95, file=stream)


def main() -> int:
    parser = argparse.ArgumentParser(description="Acceptance gate comparison.")
    parser.add_argument("ours", type=Path, help="Path to ours gate.json")
    parser.add_argument("baseline", nargs="?", type=Path, default=None,
                        help="Path to baseline gate.json (defaults to latest in bench/results/main-sweeps/)")
    parser.add_argument("--allow-deadline-noise", type=int, default=0,
                        help="Allow up to N unrouted noise on java_timeout boards (default: 0)")
    parser.add_argument("--json", type=Path, default=None,
                        help="Save comparison summary to json")
    parser.add_argument("--repo-root", type=Path, default=None,
                        help="Root of fast-routing repository")

    args = parser.parse_args()

    repo_root = args.repo_root
    if repo_root is None:
        # Default: repo root is 2 levels up from bench/tools/accept_compare.py
        repo_root = Path(__file__).resolve().parents[2]

    base_path = args.baseline
    if base_path is None:
        base_path = find_latest_baseline(repo_root)

    if not args.ours.is_file():
        print(f"Error: ours file not found: {args.ours}", file=sys.stderr)
        return 2
    if not base_path.is_file():
        print(f"Error: baseline file not found: {base_path}", file=sys.stderr)
        return 2

    rows, summary = compare_gates(
        args.ours,
        base_path,
        allow_deadline_noise=args.allow_deadline_noise,
    )

    print_comparison_table(rows, summary)

    if args.json:
        args.json.parent.mkdir(parents=True, exist_ok=True)
        with open(args.json, "w", encoding="utf-8") as f:
            json.dump({"summary": summary, "rows": rows}, f, indent=2)

    return 0 if summary["can_merge"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
