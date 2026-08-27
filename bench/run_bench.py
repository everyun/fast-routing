#!/usr/bin/env python3
"""Benchmark harness: Java Freerouting (reference) vs Rust fr-cli.

Runs both routers headlessly over the upstream fixture suite, captures
performance + quality metrics for each (tool, fixture) pair, cross-checks the
produced .ses files with the independent coarse checker (check_ses.py), and
writes results.json + report.md into bench/results/<timestamp>/.

Stdlib only. See bench/README.md for metric definitions and caveats.

Typical usage (from the repo root):

    python3 bench/run_bench.py                       # full top-level suite
    python3 bench/run_bench.py --fixtures 'Issue026*' --fixtures 'Issue143*'
    python3 bench/run_bench.py --only-rust --passes 5 --timeout 60
"""

from __future__ import annotations

import argparse
import datetime as _dt
import glob
import json
import math
import os
import re
import signal
import subprocess
import sys
import time

import check_ses

# --------------------------------------------------------------------------
# Defaults for this machine (all overridable on the CLI)
# --------------------------------------------------------------------------
REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEFAULT_FIXTURES_DIR = "/tmp/freerouting-upstream/fixtures"
DEFAULT_JAVA = "/opt/homebrew/opt/openjdk/bin/java"
DEFAULT_JAR = "/tmp/frbase/freerouting-2.3.0.jar"
DEFAULT_RUST_BIN = os.path.join(REPO_ROOT, "target", "release", "fr-cli")
TIME_BIN = "/usr/bin/time"  # `-l` prints max RSS in BYTES on this macOS (verified)

# --------------------------------------------------------------------------
# Log parsers
# --------------------------------------------------------------------------

# /usr/bin/time -l footer (goes to stderr)
RE_TIME_REAL = re.compile(r"([\d.]+)\s+real\s+([\d.]+)\s+user\s+([\d.]+)\s+sys")
RE_MAX_RSS = re.compile(r"(\d+)\s+maximum resident set size")

# Java Freerouting (with -l en). Examples seen with v2.3.0:
#   Auto-routing pass #4 on board '...' was completed in 0.05 seconds with the
#     score of 999.99 (0 unrouted and 0 violations), using 0.97 CPU seconds ...
#   Auto-routing stage completed: started with 33 unrouted nets, completed in
#     1.49 seconds, final score: 999.99 (0 unrouted and 0 violations), using
#     0.97 total CPU seconds, 5.89 GB total allocated, and 973.5 MB peak heap usage.
#   Fanout stage completed: ... completed in 0.56 seconds, ... 764.9 MB peak heap usage.
RE_JAVA_PASS = re.compile(
    r"pass #(\d+).*?completed in ([\d.]+) seconds.*?"
    r"score of ([\d.,]+) \((\d+) unrouted and (\d+) violations\)",
    re.IGNORECASE,
)
RE_JAVA_STAGE = re.compile(
    r"(\S+(?:\s\S+)*?) stage completed:.*?completed in ([\d.]+) seconds",
    re.IGNORECASE,
)
RE_JAVA_FINAL_SCORE = re.compile(
    r"final score: ([\d.,]+) \((\d+) unrouted and (\d+) violations\)",
    re.IGNORECASE,
)
RE_JAVA_CPU = re.compile(r"([\d.]+) total CPU seconds", re.IGNORECASE)
RE_JAVA_HEAP = re.compile(r"([\d.]+) (GB|MB|KB) peak heap usage", re.IGNORECASE)
# Localization fallback: the "(N unrouted and M violations)" fragment anywhere.
RE_UNROUTED_ANY = re.compile(r"\((\d+) unrouted and (\d+) violations\)")

# Rust fr-cli:
#   Routing complete in 0.02s: 14/24 nets routed (58.3%), 27 vias,
#   DRC violations: 0
RE_RUST_COMPLETE = re.compile(
    r"Routing complete in ([\d.]+)s: (\d+)/(\d+) nets routed \(([\d.]+)%\), "
    r"(\d+) vias, DRC violations: (\d+)"
)

_HEAP_MULT = {"KB": 1024, "MB": 1024**2, "GB": 1024**3}


def _num(s: str) -> float:
    """Parse a number that may use ',' as decimal separator (locale safety)."""
    return float(s.replace(",", "."))


def parse_time_footer(stderr: str) -> dict:
    out = {"max_rss_bytes": None, "time_real_s": None, "time_user_s": None,
           "time_sys_s": None}
    m = RE_TIME_REAL.search(stderr)
    if m:
        out["time_real_s"] = float(m.group(1))
        out["time_user_s"] = float(m.group(2))
        out["time_sys_s"] = float(m.group(3))
    m = RE_MAX_RSS.search(stderr)
    if m:
        out["max_rss_bytes"] = int(m.group(1))
    return out


def parse_java_log(stdout: str) -> dict:
    out = {
        "tool_time_s": None,       # sum of stage "completed in X seconds"
        "tool_cpu_s": None,        # sum of per-stage total CPU seconds
        "peak_heap_bytes": None,   # max over stages, self-reported
        "unrouted": None,
        "violations": None,
        "score": None,
        "passes_run": 0,
        "stages": {},
        "parse_ok": False,
    }
    stage_time = 0.0
    saw_stage = False
    for m in RE_JAVA_STAGE.finditer(stdout):
        saw_stage = True
        name = m.group(1).strip().lower().replace(" ", "-")
        secs = float(m.group(2))
        out["stages"][name] = out["stages"].get(name, 0.0) + secs
        stage_time += secs
    if saw_stage:
        out["tool_time_s"] = round(stage_time, 3)

    cpu = [float(m.group(1)) for m in RE_JAVA_CPU.finditer(stdout)]
    if cpu:
        out["tool_cpu_s"] = round(sum(cpu), 3)

    heaps = [
        _num(m.group(1)) * _HEAP_MULT[m.group(2).upper()]
        for m in RE_JAVA_HEAP.finditer(stdout)
    ]
    if heaps:
        out["peak_heap_bytes"] = int(max(heaps))

    passes = list(RE_JAVA_PASS.finditer(stdout))
    out["passes_run"] = len(passes)

    # Preferred: final score from the auto-routing stage-completed line
    # (take the LAST occurrence — the last stage reflects the final board).
    finals = list(RE_JAVA_FINAL_SCORE.finditer(stdout))
    if finals:
        m = finals[-1]
        out["score"] = _num(m.group(1))
        out["unrouted"] = int(m.group(2))
        out["violations"] = int(m.group(3))
        out["parse_ok"] = True
    elif passes:  # fall back to the last pass line
        m = passes[-1]
        out["score"] = _num(m.group(3))
        out["unrouted"] = int(m.group(4))
        out["violations"] = int(m.group(5))
        out["parse_ok"] = True
    else:
        # Localized/unknown log format: grab the bare "(N unrouted and M
        # violations)" fragment if it exists anywhere.
        anym = list(RE_UNROUTED_ANY.finditer(stdout))
        if anym:
            out["unrouted"] = int(anym[-1].group(1))
            out["violations"] = int(anym[-1].group(2))
            out["parse_ok"] = True
    return out


def parse_rust_log(stdout: str) -> dict:
    out = {
        "tool_time_s": None,
        "unrouted": None,
        "violations": None,
        "routed_nets": None,
        "total_nets": None,
        "vias_reported": None,
        "parse_ok": False,
    }
    m = RE_RUST_COMPLETE.search(stdout)
    if m:
        out["tool_time_s"] = float(m.group(1))
        out["routed_nets"] = int(m.group(2))
        out["total_nets"] = int(m.group(3))
        out["unrouted"] = out["total_nets"] - out["routed_nets"]
        out["vias_reported"] = int(m.group(5))
        out["violations"] = int(m.group(6))
        out["parse_ok"] = True
    return out


# --------------------------------------------------------------------------
# Running one (tool, fixture)
# --------------------------------------------------------------------------


def run_one(tool: str, fixture: str, out_dir: str, args) -> dict:
    stem = os.path.splitext(os.path.basename(fixture))[0]
    ses_dir = os.path.join(out_dir, "ses")
    log_dir = os.path.join(out_dir, "logs")
    os.makedirs(ses_dir, exist_ok=True)
    os.makedirs(log_dir, exist_ok=True)
    ses_path = os.path.join(ses_dir, f"{stem}.{tool}.ses")

    if tool == "java":
        cmd = [args.java, "-jar", args.jar,
               "-de", fixture, "-do", ses_path,
               "-mp", str(args.passes), "-l", "en"]
        ok_exit_codes = {0}
    else:
        cmd = [args.rust_bin, "-de", fixture, "-do", ses_path,
               "-mp", str(args.passes)]
        # fr-cli exits 2 when routing finished with incompletes/violations —
        # that is a result, not a crash.
        ok_exit_codes = {0, 2}

    full_cmd = [TIME_BIN, "-l"] + cmd

    rec = {
        "tool": tool,
        "fixture": os.path.basename(fixture),
        "fixture_path": fixture,
        "fixture_bytes": os.path.getsize(fixture),
        "passes": args.passes,
        "timeout_s": args.timeout,
        "cmd": full_cmd,
        "timed_out": False,
        "crashed": False,
        "exit_code": None,
        "wall_s": None,
        "ses_path": None,
        "ses_bytes": None,
        "check": None,
        "check_error": None,
    }

    t0 = time.monotonic()
    proc = subprocess.Popen(
        full_cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        errors="replace",
        start_new_session=True,  # so we can kill the whole group on timeout
    )
    try:
        stdout, stderr = proc.communicate(timeout=args.timeout)
        rec["exit_code"] = proc.returncode
    except subprocess.TimeoutExpired:
        rec["timed_out"] = True
        try:
            os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass
        stdout, stderr = proc.communicate()
        rec["exit_code"] = proc.returncode
    rec["wall_s"] = round(time.monotonic() - t0, 3)

    with open(os.path.join(log_dir, f"{stem}.{tool}.stdout.txt"), "w") as f:
        f.write(stdout)
    with open(os.path.join(log_dir, f"{stem}.{tool}.stderr.txt"), "w") as f:
        f.write(stderr)

    rec.update(parse_time_footer(stderr))
    parsed = parse_java_log(stdout) if tool == "java" else parse_rust_log(stdout)
    rec.update(parsed)

    if not rec["timed_out"]:
        signalled = rec["exit_code"] is not None and rec["exit_code"] < 0
        bad_exit = rec["exit_code"] not in ok_exit_codes
        rec["crashed"] = signalled or bad_exit or not parsed["parse_ok"]
        if rec["crashed"]:
            rec["stderr_tail"] = "\n".join(stderr.splitlines()[-15:])

    if os.path.exists(ses_path):
        rec["ses_path"] = ses_path
        rec["ses_bytes"] = os.path.getsize(ses_path)
        try:
            rec["check"] = check_ses.check(fixture, ses_path)
        except Exception as e:  # checker must never take down the harness
            rec["check_error"] = f"{type(e).__name__}: {e}"
    return rec


# --------------------------------------------------------------------------
# Reporting
# --------------------------------------------------------------------------


def _fmt_rss(b):
    return f"{b / 1024**2:.0f}M" if b is not None else "-"


def _fmt_uv(rec):
    if rec is None:
        return "-"
    if rec["timed_out"]:
        return "TIMEOUT"
    if rec["crashed"]:
        return "CRASH"
    u = rec.get("unrouted")
    v = rec.get("violations")
    return f"{u if u is not None else '?'}/{v if v is not None else '?'}"


def _fmt_time(rec):
    if rec is None or rec["timed_out"]:
        return "-"
    wall = rec["wall_s"]
    tool = rec.get("tool_time_s")
    return f"{wall:.2f} ({tool:.2f})" if tool is not None else f"{wall:.2f}"


def _fmt_chk(rec):
    if rec is None or not rec.get("check"):
        return "-"
    c = rec["check"]
    return f"{c['ses_nets_routed']}/{c['dsn_nets_multi_pin']}"


def _geomean(ratios):
    ratios = [r for r in ratios if r and r > 0]
    if not ratios:
        return None
    return math.exp(sum(math.log(r) for r in ratios) / len(ratios))


def write_report(records, out_dir, args):
    by_fixture: dict = {}
    for r in records:
        by_fixture.setdefault(r["fixture"], {})[r["tool"]] = r
    fixtures = sorted(by_fixture)

    lines = []
    lines.append("# Freerouting (Java) vs fr-cli (Rust) benchmark")
    lines.append("")
    lines.append(f"- Date: {_dt.datetime.now().isoformat(timespec='seconds')}")
    lines.append(f"- Passes: {args.passes}, timeout: {args.timeout}s")
    lines.append(f"- Jar: `{args.jar}`")
    lines.append(f"- Rust binary: `{args.rust_bin}`")
    try:
        git_rev = subprocess.run(
            ["git", "-C", REPO_ROOT, "rev-parse", "--short", "HEAD"],
            capture_output=True, text=True, timeout=10,
        ).stdout.strip()
        if git_rev:
            lines.append(f"- Repo rev: `{git_rev}`")
    except Exception:
        pass
    lines.append("")
    lines.append(
        "Cells: `u/v` = unrouted/violations (self-reported); "
        "`time` = wall s (tool-reported routing s in parens — for Java this "
        "excludes JVM startup); `RSS` = peak resident set; "
        "`chk` = independent checker, nets-with-routing / multi-pin nets in "
        "DSN (coarse, no connectivity verification)."
    )
    lines.append("")
    lines.append(
        "| fixture | java u/v | java time s | java RSS | java chk "
        "| rust u/v | rust time s | rust RSS | rust chk | wall speedup J/R |"
    )
    lines.append("|---|---|---|---|---|---|---|---|---|---|")

    wall_ratios, tool_ratios = [], []
    for fx in fixtures:
        j = by_fixture[fx].get("java")
        r = by_fixture[fx].get("rust")
        speed = "-"
        if (
            j and r
            and not (j["timed_out"] or j["crashed"] or r["timed_out"] or r["crashed"])
            and j["wall_s"] and r["wall_s"]
        ):
            ratio = j["wall_s"] / r["wall_s"]
            wall_ratios.append(ratio)
            speed = f"{ratio:.1f}x"
            if j.get("tool_time_s") and r.get("tool_time_s"):
                tool_ratios.append(j["tool_time_s"] / r["tool_time_s"])
        lines.append(
            f"| {fx} | {_fmt_uv(j)} | {_fmt_time(j)} | "
            f"{_fmt_rss(j['max_rss_bytes']) if j else '-'} | {_fmt_chk(j)} | "
            f"{_fmt_uv(r)} | {_fmt_time(r)} | "
            f"{_fmt_rss(r['max_rss_bytes']) if r else '-'} | {_fmt_chk(r)} | "
            f"{speed} |"
        )

    lines.append("")
    lines.append("## Summary")
    lines.append("")
    for tool in ("java", "rust"):
        recs = [r for r in records if r["tool"] == tool]
        if not recs:
            continue
        done = [r for r in recs if not r["timed_out"] and not r["crashed"]]
        timeouts = sum(1 for r in recs if r["timed_out"])
        crashes = sum(1 for r in recs if r["crashed"])
        unrouted = sum(r["unrouted"] for r in done if r.get("unrouted") is not None)
        viol = sum(r["violations"] for r in done if r.get("violations") is not None)
        wall = sum(r["wall_s"] for r in done)
        chk_routed = sum(
            r["check"]["ses_nets_routed"] for r in done if r.get("check")
        )
        chk_total = sum(
            r["check"]["dsn_nets_multi_pin"] for r in done if r.get("check")
        )
        rate = f"{100 * chk_routed / chk_total:.1f}%" if chk_total else "n/a"
        lines.append(
            f"- **{tool}**: {len(done)}/{len(recs)} fixtures finished "
            f"({timeouts} timeouts, {crashes} crashes); "
            f"total unrouted {unrouted}, violations {viol}; "
            f"total wall {wall:.1f}s; "
            f"checker completion {chk_routed}/{chk_total} nets ({rate})"
        )
    gw = _geomean(wall_ratios)
    gt = _geomean(tool_ratios)
    lines.append(
        f"- **Geometric-mean speedup (rust vs java)**: "
        f"wall-clock {gw:.1f}x" + (f", tool-reported {gt:.1f}x" if gt else "")
        if gw
        else "- **Geometric-mean speedup**: n/a (no fixture finished on both tools)"
    )
    lines.append("")
    lines.append(
        "> Wall-clock includes JVM startup (~10s+) which dominates small "
        "boards; the tool-reported number is the fairer algorithmic "
        "comparison. Checker completion is coarse — see check_ses.py caveats."
    )
    report = "\n".join(lines) + "\n"
    with open(os.path.join(out_dir, "report.md"), "w") as f:
        f.write(report)
    return report


# --------------------------------------------------------------------------
# Main
# --------------------------------------------------------------------------


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--fixtures-dir", default=DEFAULT_FIXTURES_DIR)
    ap.add_argument(
        "--fixtures", action="append", default=None, metavar="GLOB",
        help="glob (relative to --fixtures-dir) selecting fixtures; "
             "repeatable; default: all top-level *.dsn",
    )
    ap.add_argument("--only-rust", action="store_true")
    ap.add_argument("--only-java", action="store_true")
    ap.add_argument("--passes", type=int, default=20)
    ap.add_argument("--timeout", type=float, default=300.0,
                    help="per-run timeout in seconds (default 300)")
    ap.add_argument("--java", default=DEFAULT_JAVA)
    ap.add_argument("--jar", default=DEFAULT_JAR)
    ap.add_argument("--rust-bin", default=DEFAULT_RUST_BIN)
    ap.add_argument("--out-root",
                    default=os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                         "results"))
    args = ap.parse_args()

    globs = args.fixtures or ["*.dsn"]
    fixtures = sorted(
        {
            p
            for g in globs
            for p in glob.glob(os.path.join(args.fixtures_dir, g))
            if os.path.isfile(p) and p.lower().endswith(".dsn")
        }
    )
    if not fixtures:
        print(f"error: no fixtures matched {globs} in {args.fixtures_dir}",
              file=sys.stderr)
        return 2

    tools = []
    if not args.only_rust:
        tools.append("java")
    if not args.only_java:
        tools.append("rust")
    if not tools:
        print("error: --only-rust and --only-java are mutually exclusive",
              file=sys.stderr)
        return 2

    if "rust" in tools and not os.path.exists(args.rust_bin):
        print(f"error: rust binary not found at {args.rust_bin} "
              f"(build with: cargo build --release -p fr-cli)", file=sys.stderr)
        return 2
    if "java" in tools and not os.path.exists(args.jar):
        print(f"error: jar not found at {args.jar}", file=sys.stderr)
        return 2

    stamp = _dt.datetime.now().strftime("%Y%m%d-%H%M%S")
    out_dir = os.path.join(args.out_root, stamp)
    os.makedirs(out_dir, exist_ok=True)
    print(f"results dir: {out_dir}")
    print(f"{len(fixtures)} fixtures x {tools}, passes={args.passes}, "
          f"timeout={args.timeout}s")

    records = []
    for i, fx in enumerate(fixtures, 1):
        for tool in tools:
            print(f"[{i}/{len(fixtures)}] {tool:5s} {os.path.basename(fx)} ... ",
                  end="", flush=True)
            rec = run_one(tool, fx, out_dir, args)
            records.append(rec)
            status = ("TIMEOUT" if rec["timed_out"]
                      else "CRASH" if rec["crashed"]
                      else f"u/v={rec.get('unrouted')}/{rec.get('violations')}")
            print(f"{status}  wall={rec['wall_s']:.2f}s "
                  f"rss={_fmt_rss(rec['max_rss_bytes'])}")
            # Persist incrementally so a partial suite still leaves data.
            with open(os.path.join(out_dir, "results.json"), "w") as f:
                json.dump(records, f, indent=2)

    report = write_report(records, out_dir, args)
    print()
    print(report)
    print(f"wrote {os.path.join(out_dir, 'results.json')}")
    print(f"wrote {os.path.join(out_dir, 'report.md')}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
