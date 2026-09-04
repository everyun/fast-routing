#!/usr/bin/env python3
"""6-board CI smoke: rust-only, referee-judged, against bench/ci-baseline.json.

Runs `bench/run_bench.py --only-rust` over the boards listed in the baseline,
then calls `g21-referee` on each (dsn, ses) pair. Gates:

  * added_violations == 0  (judged charged − unmodified import)
  * unrouted <= baseline[board].unrouted_max
  * ses_errors == 0

In-tool / wall time is recorded and printed, never gated — CI machines are
not comparable. `--update-baseline` rewrites unrouted_max from this run.

    python3 bench/tools/ci_smoke.py
    python3 bench/tools/ci_smoke.py --update-baseline
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time

TOOLS_DIR = os.path.dirname(os.path.abspath(__file__))
BENCH_DIR = os.path.dirname(TOOLS_DIR)
REPO_ROOT = os.path.dirname(BENCH_DIR)

sys.path.insert(0, TOOLS_DIR)
sys.path.insert(0, BENCH_DIR)

import g21_judge  # noqa: E402

DEFAULT_BASELINE = os.path.join(BENCH_DIR, "ci-baseline.json")
DEFAULT_FIXTURES = "/tmp/freerouting-upstream/fixtures"
DEFAULT_REFEREE = os.path.join(REPO_ROOT, "target", "release", "g21-referee")
DEFAULT_RUST = os.path.join(REPO_ROOT, "target", "release", "fr-cli")
RUN_BENCH = os.path.join(BENCH_DIR, "run_bench.py")


def load_baseline(path: str) -> dict:
    with open(path, encoding="utf-8") as f:
        data = json.load(f)
    boards = data.get("boards")
    if not isinstance(boards, dict) or not boards:
        raise SystemExit(f"baseline {path} has no boards")
    return data


def evaluate(base: dict, ours: dict, unrouted_max: int) -> dict:
    """Pure gate. Referee dicts in, verdict out. No subprocess."""
    row = {
        "ok": True,
        "reasons": [],
        "added_violations": None,
        "unrouted": None,
        "ses_errors": None,
        "in_tool_s": None,
    }
    if not base or "error" in base:
        row["ok"] = False
        row["reasons"].append(f"baseline referee failed: {(base or {}).get('error', 'empty')}")
        return row
    if not ours or "error" in ours:
        row["ok"] = False
        row["reasons"].append(f"session referee failed: {(ours or {}).get('error', 'empty')}")
        return row
    ses_errors = ours.get("ses_errors") or 0
    row["ses_errors"] = ses_errors
    if ses_errors:
        row["ok"] = False
        row["reasons"].append(f"ses_errors={ses_errors}")
    charged_ours = ours.get("violations_charged")
    charged_base = base.get("violations_charged")
    if charged_ours is None or charged_base is None:
        row["ok"] = False
        row["reasons"].append("referee omitted violations_charged")
        return row
    added = charged_ours - charged_base
    row["added_violations"] = added
    if added != 0:
        row["ok"] = False
        row["reasons"].append(f"added_violations={added} (want 0)")
    unrouted = ours.get("incomplete")
    row["unrouted"] = unrouted
    if unrouted is None:
        row["ok"] = False
        row["reasons"].append("referee omitted incomplete")
    elif unrouted > unrouted_max:
        row["ok"] = False
        row["reasons"].append(f"unrouted {unrouted} > baseline max {unrouted_max}")
    return row


def run_bench(boards: list[str], args) -> str:
    out_root = args.out_root
    os.makedirs(out_root, exist_ok=True)
    cmd = [
        sys.executable,
        RUN_BENCH,
        "--only-rust",
        "--passes", str(args.passes),
        "--timeout", str(args.timeout),
        "--fixtures-dir", args.fixtures_dir,
        "--rust-bin", args.rust_bin,
        "--out-root", out_root,
        "--config", "ci-smoke",
        "--concurrency", "1",
    ]
    for board in boards:
        cmd.extend(["--fixtures", board])
    print("run_bench:", " ".join(cmd), flush=True)
    t0 = time.monotonic()
    proc = subprocess.run(cmd)
    elapsed = time.monotonic() - t0
    print(f"run_bench exited {proc.returncode} in {elapsed:.1f}s", flush=True)
    if proc.returncode != 0:
        raise SystemExit(f"run_bench.py failed with {proc.returncode}")
    # Newest stamp under out_root.
    stamps = [
        os.path.join(out_root, name)
        for name in os.listdir(out_root)
        if os.path.isdir(os.path.join(out_root, name))
        and os.path.isfile(os.path.join(out_root, name, "results.json"))
    ]
    if not stamps:
        raise SystemExit(f"run_bench produced no results.json under {out_root}")
    stamps.sort(key=lambda p: os.path.getmtime(p), reverse=True)
    return stamps[0]


def load_results(stamp_dir: str) -> dict[str, dict]:
    path = os.path.join(stamp_dir, "results.json")
    data = json.load(open(path, encoding="utf-8"))
    recs = data.get("records", data) if isinstance(data, dict) else data
    by_name = {}
    for rec in recs:
        if rec.get("tool") == "rust":
            by_name[rec["fixture"]] = rec
    return by_name


def judge_board(name: str, rec: dict, fixtures_dir: str, referee: str, timeout: float) -> tuple[dict, dict]:
    dsn = rec.get("fixture_path") or os.path.join(fixtures_dir, name)
    ses = rec.get("ses_path")
    base = g21_judge.run_referee(referee, dsn, None, timeout)
    ours = g21_judge.run_referee(referee, dsn, ses, timeout) if ses else {"error": "no session produced"}
    return base, ours


def write_baseline(path: str, original: dict, rows: list[dict]) -> None:
    boards = dict(original.get("boards") or {})
    for row in rows:
        name = row["fixture"]
        if name not in boards:
            boards[name] = {}
        if row.get("unrouted") is not None:
            boards[name]["unrouted_max"] = row["unrouted"]
    out = dict(original)
    out["boards"] = boards
    with open(path, "w", encoding="utf-8") as f:
        json.dump(out, f, indent=2)
        f.write("\n")
    print(f"wrote {path}", flush=True)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--baseline", default=DEFAULT_BASELINE)
    ap.add_argument("--fixtures-dir", default=DEFAULT_FIXTURES)
    ap.add_argument("--referee", default=DEFAULT_REFEREE)
    ap.add_argument("--rust-bin", default=DEFAULT_RUST)
    ap.add_argument("--out-root", default=os.path.join(BENCH_DIR, "results", "ci-smoke"))
    ap.add_argument("--stamp", default=None, help="reuse an existing run_bench stamp dir")
    ap.add_argument("--passes", type=int, default=None)
    ap.add_argument("--timeout", type=float, default=None)
    ap.add_argument("--referee-timeout", type=float, default=120.0)
    ap.add_argument("--update-baseline", action="store_true")
    ap.add_argument("--json-out", default=None)
    args = ap.parse_args(argv)

    baseline = load_baseline(args.baseline)
    if args.passes is None:
        args.passes = int(baseline.get("passes") or 20)
    if args.timeout is None:
        args.timeout = float(baseline.get("timeout_s") or 60)
    boards = list(baseline["boards"].keys())

    for name in boards:
        path = os.path.join(args.fixtures_dir, name)
        if not os.path.isfile(path):
            print(f"error: fixture missing: {path}", file=sys.stderr)
            return 2
    if not os.path.isfile(args.referee):
        print(f"error: referee missing: {args.referee} (cargo build --release -p g21-referee)",
              file=sys.stderr)
        return 2
    if not args.stamp and not os.path.isfile(args.rust_bin):
        print(f"error: rust bin missing: {args.rust_bin}", file=sys.stderr)
        return 2

    stamp = args.stamp or run_bench(boards, args)
    recs = load_results(stamp)
    rows = []
    all_ok = True
    print()
    print(f"{'fixture':<48} {'added':>6} {'unr':>5} {'max':>5} {'wall_s':>8}  verdict")
    for name in boards:
        rec = recs.get(name)
        ceiling = int(baseline["boards"][name]["unrouted_max"])
        if rec is None:
            row = {
                "fixture": name,
                "ok": False,
                "reasons": ["no rust record in results.json"],
                "added_violations": None,
                "unrouted": None,
                "wall_s": None,
            }
        else:
            base, ours = judge_board(name, rec, args.fixtures_dir, args.referee, args.referee_timeout)
            row = evaluate(base, ours, ceiling)
            row["fixture"] = name
            row["wall_s"] = rec.get("wall_s")
            row["tool_time_s"] = rec.get("tool_time_s")
            row["timed_out"] = rec.get("timed_out")
            row["exit_code"] = rec.get("exit_code")
            row["ses_path"] = rec.get("ses_path")
            row["stamp"] = stamp
        rows.append(row)
        all_ok = all_ok and row["ok"]
        added = row.get("added_violations")
        unr = row.get("unrouted")
        wall = row.get("wall_s")
        mark = "PASS" if row["ok"] else "FAIL"
        extra = "" if row["ok"] else "  " + "; ".join(row.get("reasons") or [])
        print(
            f"{name:<48} {str(added):>6} {str(unr):>5} {ceiling:>5} "
            f"{'' if wall is None else f'{wall:.3f}':>8}  {mark}{extra}"
        )

    payload = {
        "ok": all_ok,
        "stamp": stamp,
        "passes": args.passes,
        "timeout_s": args.timeout,
        "concurrency": 1,
        "rows": rows,
    }
    json_path = args.json_out or os.path.join(stamp, "ci-smoke.json")
    with open(json_path, "w", encoding="utf-8") as f:
        json.dump(payload, f, indent=2)
        f.write("\n")
    print(f"\nwrote {json_path}")
    if args.update_baseline:
        write_baseline(args.baseline, baseline, rows)
    if not all_ok:
        print("SMOKE FAIL", file=sys.stderr)
        return 1
    print("SMOKE PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
