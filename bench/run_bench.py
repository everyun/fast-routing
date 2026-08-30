#!/usr/bin/env python3
"""Benchmark harness: Java Freerouting (reference) vs Rust fr-cli.

Runs both routers headlessly over the upstream fixture suite, captures
performance + quality metrics for each (tool, fixture) pair, cross-checks the
produced .ses files with the independent coarse checker (check_ses.py), and
writes results.json + report.md into bench/results/<timestamp>/.

Three rules this harness enforces, from docs/ARCHITECTURE.md "Benchmark
discipline" and docs/ROADMAP.md G3.1:

  * every row is classified from bench/fixtures-manifest.json
    (`java_finished / java_timeout / java_crash / java_degenerate`), because a
    board Java timed out on, crashed on, or never had nets to route cannot
    back a speed claim;
  * the headline geomean speedup is computed **only** over fixtures that pass
    the quality predicate in the same run, and is labelled `quality-filtered`.
    An unfiltered geomean is printed only on explicit request, tagged
    `quality-unfiltered, not comparable`.
  * **a timing that was taken under contention may not become a ratio.** See
    below.

Stdlib only. See bench/README.md for metric definitions and caveats.

Serial-perf mode (`--serial-perf`)
----------------------------------
This script runs its own fixtures one at a time, but a *sweep* usually runs
several copies of it at once — the 46-board suite is normally sharded four
ways, `W=4`, because the quality gate only needs the sessions and four shards
take a quarter of the wall time. Those two uses are not interchangeable:

  * **quality** (unrouted, violations, session integrity) is a property of the
    board that comes out, and W does not touch it. Shard away;
  * **timing** is a property of the machine, and at W=4 every number on a
    28-core laptop is inflated by contention for cores, memory bandwidth and
    the page cache — unevenly, so it does not even cancel in a ratio.

So each record carries the `concurrency` it was taken at, the report labels
every timing row with it, and **rows taken at W > 1 are excluded from the
geomean** whatever the quality verdict says. `--serial-perf` is the positive
form of the same rule: it refuses to run at all unless `--concurrency 1`, which
is what a measurement run should pass so that a mistake is an error rather than
a number nobody can retract. Declare the real W with `--concurrency N` when
sharding; the default of 1 is a claim, so declaring it wrongly is on the caller.

Typical usage (from the repo root):

    python3 bench/run_bench.py                       # full top-level suite
    python3 bench/run_bench.py --fixtures 'Issue026*' --fixtures 'Issue143*'
    python3 bench/run_bench.py --only-rust --passes 5 --timeout 60
    python3 bench/run_bench.py --jar LAB.jar --passes 9999 --config mp9999-lab
    python3 bench/run_bench.py --render-from bench/results/<stamp> --report-out -
    python3 bench/run_bench.py --serial-perf --fixtures 'Issue026*'   # timings
    python3 bench/run_bench.py --concurrency 4 --fixtures 'Issue2*'   # a shard
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
import manifest as fixture_manifest

# --------------------------------------------------------------------------
# Defaults for this machine (all overridable on the CLI)
# --------------------------------------------------------------------------
REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEFAULT_FIXTURES_DIR = "/tmp/freerouting-upstream/fixtures"
DEFAULT_JAVA = "/opt/homebrew/opt/openjdk/bin/java"
DEFAULT_JAR = "/tmp/frbase/freerouting-2.3.0.jar"
DEFAULT_RUST_BIN = os.path.join(REPO_ROOT, "target", "release", "fr-cli")
DEFAULT_MANIFEST = fixture_manifest.DEFAULT_MANIFEST
TIME_BIN = "/usr/bin/time"  # `-l` prints max RSS in BYTES on this macOS (verified)

# How far under `--timeout` fr-cli's own `--job-timeout` is set.
#
# This is a **safety net, not the mechanism**. It used to be the mechanism: the
# gap had to cover everything fr-cli still did after its own budget expired, and
# when the post-deadline tail on `Issue209-split10` grew to 11.4 s (the
# optimizer walking a 50-item failure streak past the deadline, twice) the
# 10 s gap of the time was not enough, the process was SIGKILLed mid-shutdown,
# and the run lost a routed board for no routing reason. Widening it to 15 s
# bought one sweep; at W=4 the same board overran that too.
#
# The fix is in the tool: `--job-timeout` now reserves a board-sized slice of
# its own budget for the shutdown (`fr_router::reserve_job_tail`), so fr-cli
# finishes *inside* the budget it is given instead of a few seconds past it.
# This margin stays because a harness that trusts its child completely has no
# way to notice when the child is wrong — a run that still hits `--timeout` is
# now a real failure and is recorded as one.
#
# `bench/tools/g21_judge.py` reads the resulting `wall_s`, so widening this
# shortens the routing budget rather than lengthening the run.
RUST_JOB_TIMEOUT_MARGIN_S = 15.0

# results.json layout: {"meta": {...}, "records": [...]}. Bumped whenever the
# meta block or a record field changes meaning; readers accept the pre-schema
# bare-list form too (see manifest.load_results).
#
# 3 adds `concurrency` to every record: the W the run was taken at, without
# which a timing cannot be told from a timing under contention. A record with
# no `concurrency` field is from an older run and is treated as "unknown W",
# which is excluded from ratios exactly as W > 1 is — the numbers those runs
# produced are the ones this field exists to stop being quoted.
RESULTS_SCHEMA = 3

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
# Capture only the stage word(s) right before "stage completed:", not the whole
# log-line prefix (job ids etc.) — keys must aggregate across runs.
RE_JAVA_STAGE = re.compile(
    r"(\w[\w-]*(?: \w[\w-]*)?) stage completed:.*?completed in ([\d.]+) seconds",
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

# fr-cli since M2a prints the summary as three lines instead, and the counting
# unit moved from nets to *connections* — the same unit Java's "(N unrouted and
# M violations)" uses, so the two tools stayed comparable while this parser did
# not. Without these the whole `--only-rust` sweep records `parse_ok = False`
# and every row is classified CRASH.
#
#   Imported <name> in 0.42s: 2 layers, 1139 items (278 pins, 217 traces, 27 vias)
#   Board: 1198 items (278 pins, 276 traces, 27 vias); router delta +59 traces, +0 vias
#   Router: 1 passes, 392 routed, 20 failed, 275 ripped, 0 pins fanned out
#   Router stopped on board complete (0 board restores)
#   Optimizer: 3 passes, 41 of 220 items improved, score 823.12 -> 871.40, ...
#   Referee: 0 clearance violations, 26 of 147 connections incomplete, score 823.12/1000
#   Total 36.35s
#
# `Imported ... in Xs` used to carry the whole run's elapsed time and now carries
# the import's alone; `Total` is the tool time. The old line stays as a fallback
# so sweeps recorded before that split remain parseable.
RE_RUST_TOTAL = re.compile(r"^Total ([\d.]+)s\s*$", re.M)
RE_RUST_IMPORTED = re.compile(r"^Imported .* in ([\d.]+)s: ", re.M)
RE_RUST_REFEREE = re.compile(
    r"^Referee: (\d+) clearance violations, (\d+) of (\d+) connections incomplete, "
    r"score ([\d.]+)/1000",
    re.M,
)
RE_RUST_ROUTER = re.compile(r"^Router: (\d+) passes, ", re.M)
# `vias_reported` is a quality number about the board we *finished*, so it has to
# come off the `Board:` line. The `Imported` line carries the same shape but the
# importer's counts: reading vias off it under-reports every board the router put
# a via on. The legacy pattern stays as the fallback for `--import-only` stdout
# (where the two counts are the same board) and for sweeps recorded before the
# `Board:` line existed.
RE_RUST_VIAS = re.compile(r"^Board: .*, (\d+) vias\)", re.M)
RE_RUST_VIAS_LEGACY = re.compile(r"items \(\d+ pins, \d+ traces, (\d+) vias\)")

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

    # The M2a+ three-line summary. `unrouted`/`total` are connections, not
    # nets, so `routed_nets`/`total_nets` stay `None` rather than being filled
    # with a different unit; nothing downstream reads them for the rust side.
    referee = RE_RUST_REFEREE.search(stdout)
    if referee is None:
        return out
    out["violations"] = int(referee.group(1))
    out["unrouted"] = int(referee.group(2))
    # `Total Xs` is the whole run's elapsed time (`fr-cli/src/main.rs`
    # `print_summary`), i.e. the tool time in the same sense as Java's
    # auto-routing stage total. Before that line existed the `Imported` one
    # carried it, so old stdout still parses.
    total = RE_RUST_TOTAL.search(stdout) or RE_RUST_IMPORTED.search(stdout)
    if total:
        out["tool_time_s"] = float(total.group(1))
    vias = RE_RUST_VIAS.search(stdout) or RE_RUST_VIAS_LEGACY.search(stdout)
    if vias:
        out["vias_reported"] = int(vias.group(1))
    # A routing run must have printed the `Router:` line; an `--import-only`
    # style stdout is not a routing result.
    out["parse_ok"] = RE_RUST_ROUTER.search(stdout) is not None
    return out


# --------------------------------------------------------------------------
# Quality predicates
#
# A predicate answers one question per fixture: "is the Rust result on this
# board good enough that comparing its runtime to Java's means anything?"
# Only fixtures whose predicate passes **in the same run** feed the headline
# geomean (ROADMAP G3.1). Signature:
#
#     predicate(java_rec, rust_rec, manifest_entry) -> (ok: bool, reason: str)
#
# `reason` is printed in the report's quality column for both outcomes, so a
# filtered-out board always says why it was filtered out.
# --------------------------------------------------------------------------


def _finished(rec) -> bool:
    return bool(rec) and not rec.get("timed_out") and not rec.get("crashed")


def quality_placeholder_nets(java, rust, entry):
    """Conservative stand-in until fr-check (ROADMAP M1.4) exists.

    Deliberately strict, because everything it lets through becomes a public
    speed number: the board must be one Java actually routed
    (`java_finished`), both tools must have finished, and our SES must carry
    routing for **at least as many nets** as Java's did, as counted by the
    independent checker rather than either tool's self-report.

    It is NOT a quality gate: the checker cannot see connectivity or
    clearance, so "same net count" can still mean "worse board". It only
    keeps the geomean off boards where we are visibly routing less.
    """
    cls = (entry or {}).get("java", {}).get("class", "java_unknown")
    if cls != "java_finished":
        return False, cls
    if not _finished(java):
        return False, "java did not finish"
    if not _finished(rust):
        return False, "rust did not finish"
    jc, rc = java.get("check"), rust.get("check")
    if not jc or not rc:
        return False, "no checker data"
    jn, rn = jc.get("ses_nets_routed"), rc.get("ses_nets_routed")
    if jn is None or rn is None:
        return False, "no checker net count"
    if rn < jn:
        return False, f"checker nets {rn} < java {jn}"
    return True, f"checker nets {rn} >= java {jn}"


def quality_fr_check(java, rust, entry):
    """Real gate (G2.1), wired once fr-check lands: added_violations(ours) <=
    added_violations(java) AND unrouted(ours) <= unrouted(java), both judged
    from the SES by fr-check, with the absolute gate for non-`java_finished`
    boards. Until then it passes nothing — a missing referee must never
    silently degrade into "everything passes"."""
    return False, "fr-check referee not implemented (ROADMAP M1.4)"


QUALITY_PREDICATES = {
    "placeholder-nets-v0": quality_placeholder_nets,
    "fr-check": quality_fr_check,
}
DEFAULT_QUALITY_PREDICATE = "placeholder-nets-v0"


# --------------------------------------------------------------------------
# Running one (tool, fixture)
# --------------------------------------------------------------------------


def run_one(tool: str, fixture: str, out_dir: str, args, entry=None) -> dict:
    stem = os.path.splitext(os.path.basename(fixture))[0]
    ses_dir = os.path.join(out_dir, "ses")
    log_dir = os.path.join(out_dir, "logs")
    os.makedirs(ses_dir, exist_ok=True)
    os.makedirs(log_dir, exist_ok=True)
    ses_path = os.path.join(ses_dir, f"{stem}.{tool}.ses")

    if tool == "java":
        cmd = [args.java, "-Djava.awt.headless=true",  # never steal macOS focus
               "-jar", args.jar,
               "-de", fixture, "-do", ses_path,
               "-mp", str(args.passes), "-l", "en"]
        ok_exit_codes = {0}
    else:
        cmd = [args.rust_bin, "-de", fixture, "-do", ses_path,
               "-mp", str(args.passes)]
        # Give fr-cli a job budget a little under our own cap so it stops
        # itself, restores its best board, judges it and writes the session —
        # instead of being SIGKILLed at the cap with nothing on disk, which is
        # what turned 26 of the 46 boards in the first G2.1 read into "no
        # session written". The margin covers the referee and the writer on the
        # largest fixtures; the harness still kills at --timeout if the budget
        # is somehow not honoured, and that is then a real failure.
        job_budget = args.timeout - RUST_JOB_TIMEOUT_MARGIN_S
        if job_budget > 0:
            cmd += ["--job-timeout", f"{job_budget:g}"]
        # fr-cli exits 2 when routing finished with incompletes/violations and
        # 4 when the job budget ran out — both are results, not crashes.
        ok_exit_codes = {0, 2, 4}

    full_cmd = [TIME_BIN, "-l"] + cmd

    rec = {
        "tool": tool,
        "fixture": os.path.basename(fixture),
        "fixture_path": fixture,
        "fixture_bytes": os.path.getsize(fixture),
        # Manifest classification, copied in so a results.json stays readable
        # on its own; `java_unknown` also means "no manifest was loaded".
        "java_class": (entry or {}).get("java", {}).get("class", "java_unknown"),
        "dedup_group": (entry or {}).get("dedup", {}).get("group"),
        "dedup_primary": (entry or {}).get("dedup", {}).get("primary", True),
        "variant_family": (entry or {}).get("variant_family"),
        "passes": args.passes,
        "timeout_s": args.timeout,
        "config_label": args.config,
        # The W this timing was taken at — declared by the caller, because only
        # the caller knows how many shards it started. See the module docstring.
        "concurrency": args.concurrency,
        "cmd": full_cmd,
        "timed_out": False,
        "crashed": False,
        "exit_code": None,
        "wall_s": None,
        "ses_path": None,
        "ses_bytes": None,
        "check": None,
        "check_per_net_path": None,
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
            chk = check_ses.check(fixture, ses_path)
            # The per-net breakdown (delta-gate input) is thousands of entries
            # per suite; it lives in a sidecar so results.json stays readable.
            per_net = chk.pop("per_net", None)
            if per_net is not None:
                nets_dir = os.path.join(out_dir, "nets")
                os.makedirs(nets_dir, exist_ok=True)
                per_net_path = os.path.join(nets_dir, f"{stem}.{tool}.nets.json")
                with open(per_net_path, "w", encoding="utf-8") as f:
                    json.dump(per_net, f, indent=1, ensure_ascii=False)
                rec["check_per_net_path"] = per_net_path
            rec["check"] = chk
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


def concurrency_of(rec):
    """The W a record was taken at; `None` when it predates the field.

    `None` is not 1. A run recorded before `concurrency` existed may have been
    a four-way shard, and the whole point of the field is that a timing whose
    conditions are unknown may not become a ratio.
    """
    if not rec:
        return None
    return rec.get("concurrency")


def _fmt_w(java, rust):
    """The W column: one value when both agree, `j/r` when they do not."""
    def one(rec):
        w = concurrency_of(rec)
        return "?" if w is None else str(w)

    present = [rec for rec in (java, rust) if rec]
    if not present:
        return "-"
    labels = {one(rec) for rec in present}
    return labels.pop() if len(labels) == 1 else f"{one(java)}/{one(rust)}"


def is_serial(java, rust):
    """Whether both timings were taken with nothing else running.

    A missing record is not a problem — a `--only-rust` run has no Java side
    and its ratios are `None` anyway — but a record that *is* present must say
    it ran alone.
    """
    return all(concurrency_of(rec) == 1 for rec in (java, rust) if rec)


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


def _short_class(cls):
    return (cls or "java_unknown").replace("java_", "")


def group_by_fixture(records) -> dict:
    by_fixture: dict = {}
    for r in records:
        by_fixture.setdefault(r["fixture"], {})[r["tool"]] = r
    return by_fixture


def judge(by_fixture, manifest_index, predicate_name) -> dict:
    """Per fixture: class, speed ratios, and the quality verdict.

    Byte-identical fixtures are collapsed here: one md5 dedup group
    contributes one row to the aggregates, otherwise the suite silently
    weights one design twice (the Issue110 solder-station pair). The kept
    representative is the first member *present in this run* (fixtures are
    walked in sorted order), so a subset run that contains only the
    non-primary member still gets counted once rather than zero times.
    """
    predicate = QUALITY_PREDICATES[predicate_name]
    seen_groups: dict = {}
    verdicts: dict = {}
    for fx in sorted(by_fixture):
        j = by_fixture[fx].get("java")
        r = by_fixture[fx].get("rust")
        entry = manifest_index.get(fx)
        # A record carries its own class (recorded at run time); fall back to
        # the current manifest for old results.json files.
        cls = (j or r or {}).get("java_class") or (
            (entry or {}).get("java", {}).get("class", "java_unknown")
        )
        if entry is not None:
            entry = dict(entry, java=dict(entry.get("java", {}), **{"class": cls}))
        else:
            entry = {"java": {"class": cls}}

        wall_ratio = tool_ratio = None
        if _finished(j) and _finished(r) and j.get("wall_s") and r.get("wall_s"):
            wall_ratio = j["wall_s"] / r["wall_s"]
            if j.get("tool_time_s") and r.get("tool_time_s"):
                tool_ratio = j["tool_time_s"] / r["tool_time_s"]

        ok, reason = predicate(j, r, entry)

        group = (j or r or {}).get("dedup_group")
        if group is None:
            group = ((manifest_index.get(fx) or {}).get("dedup") or {}).get("group")
        dedup_skip = group is not None and group in seen_groups
        if group is not None:
            seen_groups.setdefault(group, fx)
        if dedup_skip:
            # Reported ahead of the predicate's own verdict: the row is out of
            # the filtered aggregate however good the result was.
            ok = False
            reason = f"byte-identical duplicate of {seen_groups[group]}"

        verdicts[fx] = {
            "class": cls,
            "ok": ok,
            "reason": reason,
            "dedup_skip": dedup_skip,
            "wall_ratio": wall_ratio,
            "tool_ratio": tool_ratio,
            # Quality and timing are judged separately on purpose: a board
            # sharded four ways is a perfectly good quality result and a
            # worthless timing. `ok` above is the quality verdict and says
            # nothing about W; `serial` is the timing one and says nothing
            # about the board.
            "serial": is_serial(j, r),
            "concurrency": _fmt_w(j, r),
        }
    return verdicts


def render_report(records, meta, manifest_index, predicate_name,
                  emit_unfiltered=False) -> str:
    by_fixture = group_by_fixture(records)
    fixtures = sorted(by_fixture)
    verdicts = judge(by_fixture, manifest_index, predicate_name)

    lines = []
    lines.append("# Freerouting (Java) vs fr-cli (Rust) benchmark")
    lines.append("")
    lines.append(f"- Run: {meta.get('generated', '?')}"
                 + (f" (rendered {_dt.datetime.now().isoformat(timespec='seconds')})"
                    if meta.get("rendered_from") else ""))
    lines.append(f"- Config label: `{meta.get('config_label') or 'none'}`")
    lines.append(f"- Passes: {meta.get('passes')}, timeout: {meta.get('timeout_s')}s")
    if meta.get("concurrency") is not None:
        lines.append(
            f"- Concurrency: W={meta['concurrency']}"
            + (" (`--serial-perf`: timings are quotable)"
               if meta.get("serial_perf") else
               "" if meta["concurrency"] == 1 else
               " — **sharded sweep; the timing columns are not comparable**")
        )
    lines.append(f"- Jar: `{meta.get('jar')}`")
    lines.append(f"- Rust binary: `{meta.get('rust_bin')}`")
    if meta.get("repo_rev"):
        lines.append(f"- Repo rev: `{meta['repo_rev']}`")
    lines.append(
        f"- Manifest: `{meta.get('manifest') or 'none'}`"
        + (f" (generated {meta['manifest_generated']})"
           if meta.get("manifest_generated") else "")
        + ("" if manifest_index else
           " — **not loaded, every row is `unknown`**")
    )
    lines.append(f"- Quality predicate: `{predicate_name}`")
    if meta.get("rendered_from"):
        lines.append(f"- Rendered from: `{meta['rendered_from']}` (tools not re-run)")
    lines.append("")
    lines.append(
        "Cells: `class` = manifest classification of the **Java** reference "
        "on this board; `u/v` = unrouted/violations **as the tool reports "
        "them about itself** (no independent verification — fr-check, ROADMAP "
        "M1.4, replaces this column); `time` = wall s (tool-reported routing s "
        "in parens — for Java this excludes JVM startup); `RSS` = peak "
        "resident set; `nets(chk)` = check_ses.py, nets-with-routing / "
        "multi-pin nets in the DSN (coarse: no connectivity, no clearance); "
        "`quality` = the run's quality predicate — only `pass` rows feed the "
        "headline geomean; `W` = how many bench processes were running at once "
        "when this row was timed (`?` = a run recorded before the field "
        "existed). **Every timing on a `W > 1` row is inflated by contention "
        "and is excluded from the geomean whatever `quality` says** — see "
        "`--serial-perf`."
    )
    lines.append("")
    lines.append(
        "| fixture | class | W | java u/v (self) | java time s | java RSS "
        "| java nets(chk) | rust u/v (self) | rust time s | rust RSS "
        "| rust nets(chk) | wall J/R | in-tool J/R | quality |"
    )
    lines.append("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")

    for fx in fixtures:
        j = by_fixture[fx].get("java")
        r = by_fixture[fx].get("rust")
        v = verdicts[fx]
        wall = f"{v['wall_ratio']:.1f}x" if v["wall_ratio"] else "-"
        tool = f"{v['tool_ratio']:.1f}x" if v["tool_ratio"] else "-"
        lines.append(
            f"| {fx} | {_short_class(v['class'])} | {v['concurrency']} "
            f"| {_fmt_uv(j)} | {_fmt_time(j)} | "
            f"{_fmt_rss(j['max_rss_bytes']) if j else '-'} | {_fmt_chk(j)} | "
            f"{_fmt_uv(r)} | {_fmt_time(r)} | "
            f"{_fmt_rss(r['max_rss_bytes']) if r else '-'} | {_fmt_chk(r)} | "
            f"{wall} | {tool} | "
            f"{'pass' if v['ok'] else v['reason']} |"
        )

    lines.append("")
    lines.append("## Summary")
    lines.append("")
    for tool in ("java", "rust"):
        recs = [r for r in records if r["tool"] == tool]
        if not recs:
            continue
        done = [r for r in recs if _finished(r)]
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
            f"- **{tool}**: {len(done)}/{len(recs)} runs finished "
            f"({timeouts} timeouts, {crashes} crashes); "
            f"self-reported total unrouted {unrouted}, violations {viol}; "
            f"total wall {wall:.1f}s; "
            f"checker completion {chk_routed}/{chk_total} nets ({rate})"
        )

    class_counts: dict = {}
    for v in verdicts.values():
        class_counts[v["class"]] = class_counts.get(v["class"], 0) + 1
    lines.append(
        "- **Fixture classes** (Java reference): "
        + ", ".join(f"{k} {class_counts[k]}" for k in sorted(class_counts))
    )

    passed = [fx for fx in fixtures if verdicts[fx]["ok"]]
    dups = [fx for fx in fixtures if verdicts[fx]["dedup_skip"]]
    lines.append(
        f"- **Quality filter** `{predicate_name}`: {len(passed)}/{len(fixtures)} "
        f"fixtures pass"
        + (f"; {len(dups)} byte-identical duplicate(s) excluded from the "
           f"filtered aggregate" if dups else "")
    )
    # Quality decides which boards may be compared; W decides whether their
    # clocks mean anything. Both filters apply, and the report says which one
    # dropped what — a number withheld for the wrong reason is as useless as a
    # wrong number.
    timed = [fx for fx in passed if verdicts[fx]["serial"]]
    contended = [fx for fx in passed if not verdicts[fx]["serial"]]
    if contended:
        lines.append(
            f"- **Excluded from the geomean as non-serial** "
            f"({len(contended)} of {len(passed)} quality-passing fixtures): "
            + ", ".join(f"{fx} (W={verdicts[fx]['concurrency']})"
                        for fx in contended)
            + ". Their quality verdicts stand; their timings do not. Re-measure "
              "with `--serial-perf`."
        )
    walls = [verdicts[fx]["wall_ratio"] for fx in timed]
    tools_r = [verdicts[fx]["tool_ratio"] for fx in timed]
    gw, gt = _geomean(walls), _geomean(tools_r)
    if gw:
        n_tool = sum(1 for t in tools_r if t)
        lines.append(
            f"- **Geomean speedup (java/rust), quality-filtered, serial "
            f"(W=1)**: wall-clock {gw:.1f}x (n={sum(1 for w in walls if w)})"
            + (f", in-tool {gt:.1f}x (n={n_tool})" if gt else ", in-tool n/a")
        )
    elif passed and not timed:
        lines.append(
            f"- **Geomean speedup, quality-filtered**: n/a — "
            f"{len(passed)} fixture(s) passed `{predicate_name}` but none was "
            f"timed serially. No speed claim may be made from this run."
        )
    else:
        lines.append(
            f"- **Geomean speedup, quality-filtered**: n/a — 0 fixtures passed "
            f"`{predicate_name}`. No speed claim may be made from this run."
        )
    if passed:
        lines.append("- Quality-filtered set: " + ", ".join(passed))

    if emit_unfiltered:
        u_walls = [verdicts[fx]["wall_ratio"] for fx in fixtures
                   if verdicts[fx]["serial"]]
        u_tools = [verdicts[fx]["tool_ratio"] for fx in fixtures
                   if verdicts[fx]["serial"]]
        uw, ut = _geomean(u_walls), _geomean(u_tools)
        # Separate sample counts, as in the filtered block above: a fixture
        # whose tool-reported time is 0 contributes a wall ratio but no
        # in-tool ratio, so a single `n` mislabels one of the two figures.
        n_wall = sum(1 for w in u_walls if w and w > 0)
        n_tool = sum(1 for t in u_tools if t and t > 0)
        lines.append(
            f"- **quality-unfiltered, not comparable** (every serially timed "
            f"fixture both tools finished, no quality check whatsoever — "
            f"includes boards where we route a fraction of the nets): "
            f"wall-clock "
            + (f"{uw:.1f}x (n={n_wall})" if uw else "n/a")
            + (f", in-tool {ut:.1f}x (n={n_tool})" if ut else ", in-tool n/a")
        )

    lines.append("")
    lines.append(
        "> Wall-clock includes JVM startup (~10 s) which dominates small "
        "boards; in-tool time is the fairer algorithmic comparison and the "
        "product headline stays wall-clock with startup broken out (ROADMAP "
        "G4). Both `u/v` columns are self-reports and both `nets(chk)` "
        "columns are coarse completion counts — neither is a DRC verdict, so "
        "the quality filter above is a floor, not a gate. `degenerate` = Java "
        "finished with score 0.00 / no nets to route; those boards look like "
        "clean wins and are excluded by construction. Every ratio above is "
        "from `W=1` rows only; a sharded sweep produces quality verdicts and "
        "no speed number at all."
    )
    return "\n".join(lines) + "\n"


def write_report(records, meta, manifest_index, predicate_name, report_path,
                 emit_unfiltered=False) -> str:
    report = render_report(records, meta, manifest_index, predicate_name,
                           emit_unfiltered)
    if report_path and report_path != "-":
        os.makedirs(os.path.dirname(os.path.abspath(report_path)), exist_ok=True)
        with open(report_path, "w", encoding="utf-8") as f:
            f.write(report)
    return report


# --------------------------------------------------------------------------
# Main
# --------------------------------------------------------------------------


def _git_rev():
    try:
        rev = subprocess.run(
            ["git", "-C", REPO_ROOT, "rev-parse", "--short", "HEAD"],
            capture_output=True, text=True, timeout=10,
        ).stdout.strip()
        return rev or None
    except Exception:
        return None


def load_manifest_index(path, quiet=False):
    """(index, manifest) for --manifest; warns loudly when it is missing."""
    man = fixture_manifest.load_manifest(path) if path else None
    if man is None and not quiet:
        print(f"warning: no fixture manifest at {path} — every row will be "
              f"classified 'unknown' and the quality filter will reject "
              f"everything (run: python3 bench/manifest.py)", file=sys.stderr)
    return fixture_manifest.index_by_fixture(man), man


def resolve_results_path(target: str) -> str:
    """Accept either a results.json or the directory containing one."""
    if os.path.isdir(target):
        return os.path.join(target, "results.json")
    return target


def check_timing_flags(args) -> str | None:
    """The `--serial-perf` / `--concurrency` contract, or `None` when it holds.

    Separated from `main` so the rule is testable without a subprocess: it is
    the one place where a mistyped sweep becomes an error instead of a number
    that later has to be retracted.
    """
    if args.concurrency < 1:
        return (f"--concurrency {args.concurrency}: at least one process is "
                f"running, this one")
    if args.serial_perf and args.concurrency != 1:
        return (f"--serial-perf with --concurrency {args.concurrency}: a "
                f"timing run must have the machine to itself. Drop "
                f"--serial-perf to record this sweep for quality only, or run "
                f"the shards one at a time")
    return None


def render_only(args) -> int:
    src = resolve_results_path(args.render_from)
    if not os.path.exists(src):
        print(f"error: no results.json at {src}", file=sys.stderr)
        return 2
    meta, records = fixture_manifest.load_results(src)
    if not records:
        print(f"error: {src} contains no records", file=sys.stderr)
        return 2
    # Pre-envelope results.json is a bare list: recover what the run was from
    # the records themselves rather than inventing values.
    first = records[0]
    meta = dict(meta)
    meta.setdefault("generated", None)
    meta.setdefault("passes", first.get("passes"))
    meta.setdefault("timeout_s", first.get("timeout_s"))
    meta.setdefault("config_label", first.get("config_label"))
    meta.setdefault("concurrency", first.get("concurrency"))
    meta.setdefault("jar", next(
        (r["cmd"][r["cmd"].index("-jar") + 1] for r in records
         if r.get("tool") == "java" and "-jar" in (r.get("cmd") or [])), None))
    meta.setdefault("rust_bin", next(
        (r["cmd"][2] for r in records
         if r.get("tool") == "rust" and len(r.get("cmd") or []) > 2), None))
    if not meta.get("generated"):
        mtime = _dt.datetime.fromtimestamp(os.path.getmtime(src))
        meta["generated"] = (f"{mtime.isoformat(timespec='seconds')} "
                             f"(results.json mtime — pre-schema file)")
    meta["rendered_from"] = fixture_manifest.repo_rel(src)

    index, man = load_manifest_index(args.manifest)
    if man:
        meta["manifest"] = fixture_manifest.repo_rel(args.manifest)
        meta["manifest_generated"] = man.get("generated_utc")

    out = args.report_out or os.path.join(os.path.dirname(src), "report.md")
    report = write_report(records, meta, index, args.quality_predicate, out,
                          args.emit_unfiltered_geomean)
    if out == "-":
        sys.stdout.write(report)
    else:
        print(report)
        print(f"wrote {out}")
    return 0


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
    ap.add_argument("--jar", default=DEFAULT_JAR,
                    help=f"Freerouting jar under test (default {DEFAULT_JAR})")
    ap.add_argument("--rust-bin", default=DEFAULT_RUST_BIN)
    ap.add_argument("--config", default=None, metavar="LABEL",
                    help="free-form label for this configuration (e.g. "
                         "'mp9999-300s'), recorded in results.json and the "
                         "report so runs with different reference configs are "
                         "never compared by accident")
    ap.add_argument("--manifest", default=DEFAULT_MANIFEST,
                    help="fixture manifest supplying the per-board Java "
                         "classification (bench/manifest.py generates it)")
    ap.add_argument("--quality-predicate", default=DEFAULT_QUALITY_PREDICATE,
                    choices=sorted(QUALITY_PREDICATES),
                    help="which fixtures may feed the headline geomean "
                         f"(default {DEFAULT_QUALITY_PREDICATE})")
    ap.add_argument("--concurrency", "-W", type=int, default=1, metavar="N",
                    help="how many bench processes are running at once on this "
                         "machine, this one included (default 1). Recorded on "
                         "every record; rows taken at W > 1 are labelled in the "
                         "report and excluded from every ratio. Declare it when "
                         "you shard — the default is a claim, not a guess")
    ap.add_argument("--serial-perf", action="store_true",
                    help="this run is a timing run: refuse to start unless "
                         "--concurrency is 1. Quality sweeps do not need it; "
                         "anything whose numbers will be quoted does")
    ap.add_argument("--emit-unfiltered-geomean", action="store_true",
                    help="also print the geomean over every fixture both "
                         "tools finished, tagged 'quality-unfiltered, not "
                         "comparable'. Never quote that number.")
    ap.add_argument("--render-from", default=None, metavar="PATH",
                    help="re-render the report from an existing results.json "
                         "(or its directory) without running either tool")
    ap.add_argument("--report-out", default=None, metavar="PATH",
                    help="where to write report.md ('-' = stdout only); "
                         "default: alongside the results.json")
    ap.add_argument("--out-root",
                    default=os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                         "results"))
    args = ap.parse_args()

    problem = check_timing_flags(args)
    if problem:
        print(f"error: {problem}", file=sys.stderr)
        return 2

    if args.render_from:
        return render_only(args)

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

    index, man = load_manifest_index(args.manifest)

    stamp = _dt.datetime.now().strftime("%Y%m%d-%H%M%S")
    out_dir = os.path.join(args.out_root, stamp)
    os.makedirs(out_dir, exist_ok=True)
    print(f"results dir: {out_dir}")
    print(f"{len(fixtures)} fixtures x {tools}, passes={args.passes}, "
          f"timeout={args.timeout}s, config={args.config or 'none'}, "
          f"W={args.concurrency}"
          + (" (serial-perf: timings from this run may be quoted)"
             if args.serial_perf else
             "" if args.concurrency == 1 else
             " — timings from this run may NOT be quoted"))

    meta = {
        "schema": RESULTS_SCHEMA,
        "generated": _dt.datetime.now().isoformat(timespec="seconds"),
        "config_label": args.config,
        "tools": tools,
        "passes": args.passes,
        "timeout_s": args.timeout,
        "java": args.java,
        "jar": args.jar,
        "rust_bin": args.rust_bin,
        "fixtures_dir": args.fixtures_dir,
        "fixture_count": len(fixtures),
        "manifest": fixture_manifest.repo_rel(args.manifest) if man else None,
        "manifest_generated": (man or {}).get("generated_utc"),
        "quality_predicate": args.quality_predicate,
        "concurrency": args.concurrency,
        "serial_perf": args.serial_perf,
        "repo_rev": _git_rev(),
    }
    results_path = os.path.join(out_dir, "results.json")

    records = []
    for i, fx in enumerate(fixtures, 1):
        entry = index.get(os.path.basename(fx))
        for tool in tools:
            print(f"[{i}/{len(fixtures)}] {tool:5s} {os.path.basename(fx)} ... ",
                  end="", flush=True)
            rec = run_one(tool, fx, out_dir, args, entry)
            records.append(rec)
            status = ("TIMEOUT" if rec["timed_out"]
                      else "CRASH" if rec["crashed"]
                      else f"u/v={rec.get('unrouted')}/{rec.get('violations')}")
            print(f"{status}  wall={rec['wall_s']:.2f}s "
                  f"rss={_fmt_rss(rec['max_rss_bytes'])}")
            # Persist incrementally so a partial suite still leaves data.
            with open(results_path, "w", encoding="utf-8") as f:
                json.dump({"meta": meta, "records": records}, f, indent=2,
                          ensure_ascii=False)

    report_path = args.report_out or os.path.join(out_dir, "report.md")
    report = write_report(records, meta, index, args.quality_predicate,
                          report_path, args.emit_unfiltered_geomean)
    print()
    print(report)
    print(f"wrote {results_path}")
    if report_path != "-":
        print(f"wrote {report_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
