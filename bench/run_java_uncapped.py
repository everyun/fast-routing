#!/usr/bin/env python3
"""Hermetic uncapped (2 h cap) Java completion runs for the java_timeout boards.

Re-runs, with the clean pipeline (a fresh empty `--user_data_path` per call),
every board the current manifest classifies `java_timeout`. This replaces the
contaminated 2026-08-28 campaign archived at
`bench/results/java-uncapped-contaminated-20260828/`.

Writes (incrementally, after every board):
  bench/results/java-uncapped/<stem>.log     jar stdout + /usr/bin/time -l footer
  bench/results/java-uncapped/<stem>.ses     session, when Java writes one
  bench/results/java-uncapped/results.json   one record per board

The log layout is exactly what `manifest.parse_uncapped_log` /
`load_uncapped` already parse, so `python3 bench/manifest.py` folds the new
`uncapped` blocks in with no parser change.

The 7200 s watchdog kills only the java process (the grandchild), never the
`/usr/bin/time` wrapper, so capped runs still end with a time footer at
~7200 s wall and parse as `hit_cap`/`blocked`, same as the old campaign.

    python3 bench/run_java_uncapped.py                  # all java_timeout boards, W=2
    python3 bench/run_java_uncapped.py --fixtures 'Issue208*' --force
"""

from __future__ import annotations

import argparse
import concurrent.futures
import datetime as _dt
import glob
import json
import os
import signal
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import manifest as fixture_manifest
import run_bench

BENCH_DIR = os.path.dirname(os.path.abspath(__file__))
DEFAULT_OUT_DIR = os.path.join(BENCH_DIR, "results", "java-uncapped")
CAP_S = fixture_manifest.UNCAPPED_CAP_S

_write_lock = threading.Lock()


def kill_grandchildren(parent_pid: int) -> None:
    """SIGKILL the direct children of the /usr/bin/time wrapper (the JVM)."""
    try:
        out = subprocess.run(
            ["pgrep", "-P", str(parent_pid)], capture_output=True, text=True
        ).stdout.split()
    except OSError:
        out = []
    for pid_s in out:
        try:
            os.kill(int(pid_s), signal.SIGKILL)
        except OSError:
            pass


def run_board(fixture: str, out_dir: str, args) -> dict:
    """One uncapped run. Returns the record; writes <stem>.log (+ .ses)."""
    stem = os.path.splitext(os.path.basename(fixture))[0]
    log_path = os.path.join(out_dir, f"{stem}.log")
    ses_path = os.path.join(out_dir, f"{stem}.ses")
    user_data = run_bench.new_java_user_data_dir(
        os.path.join(out_dir, "java-user-data"), stem)
    cmd = ["/usr/bin/time", "-l"] + run_bench.build_java_cmd(
        args.java, args.jar, fixture, ses_path, args.passes, user_data)
    # nice the whole wrapper; the JVM inherits it.
    cmd = ["nice", "-n", "10"] + cmd

    started = _dt.datetime.now()
    t0 = time.monotonic()
    proc = subprocess.Popen(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=False,  # keep time+java in our group; we kill by pid
    )
    killed = False
    try:
        stdout_b, stderr_b = proc.communicate(timeout=args.timeout)
    except subprocess.TimeoutExpired:
        killed = True
        kill_grandchildren(proc.pid)
        try:
            stdout_b, stderr_b = proc.communicate(timeout=120)
        except subprocess.TimeoutExpired:
            proc.kill()
            stdout_b, stderr_b = proc.communicate()
    wall = time.monotonic() - t0
    ended = _dt.datetime.now()

    stdout = stdout_b.decode("utf-8", errors="replace")
    stderr = stderr_b.decode("utf-8", errors="replace")
    with open(log_path, "w", encoding="utf-8") as f:
        f.write(stdout)
        if not stdout.endswith("\n"):
            f.write("\n")
        f.write(stderr)

    parsed_log = run_bench.parse_java_log(stdout)
    timing = run_bench.parse_time_footer(stderr)
    uncapped = fixture_manifest.parse_uncapped_log(stdout + "\n" + stderr)
    settings = run_bench.parse_java_settings(
        stdout, cmd, timed_out=killed, parse_ok=parsed_log["parse_ok"])

    rec = {
        "tool": "java",
        "fixture": os.path.basename(fixture),
        "fixture_path": fixture,
        "passes": args.passes,
        "timeout_s": args.timeout,
        "config_label": args.config,
        "concurrency": args.workers,
        "contended": True,
        "nice": 10,
        "cmd": cmd,
        "started": started.isoformat(timespec="seconds"),
        "ended": ended.isoformat(timespec="seconds"),
        "killed_at_cap": killed,
        "exit_code": proc.returncode,
        "wall_s": round(wall, 3),
        "ses_path": ses_path if os.path.isfile(ses_path) else None,
        "ses_bytes": os.path.getsize(ses_path) if os.path.isfile(ses_path) else None,
        "log": fixture_manifest.repo_rel(log_path),
        "user_data_path": user_data,
        "java_settings": settings,
        "uncapped": uncapped,
        **{k: parsed_log[k] for k in
           ("tool_time_s", "tool_cpu_s", "peak_heap_bytes", "unrouted",
            "violations", "score", "passes_run", "stages", "parse_ok")},
        **timing,
    }
    return rec


def write_results(out_dir: str, records: list[dict], meta: dict) -> None:
    path = os.path.join(out_dir, "results.json")
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump({"meta": meta, "records": records}, f, indent=2,
                  ensure_ascii=False)
    os.replace(tmp, path)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--fixtures-dir", default=run_bench.DEFAULT_FIXTURES_DIR)
    ap.add_argument("--jar", default=run_bench.DEFAULT_JAR)
    ap.add_argument("--java", default=run_bench.DEFAULT_JAVA)
    ap.add_argument("--passes", type=int, default=20)
    ap.add_argument("--timeout", type=float, default=CAP_S)
    ap.add_argument("--workers", "-W", type=int, default=2)
    ap.add_argument("--out-dir", default=DEFAULT_OUT_DIR)
    ap.add_argument("--manifest", default=fixture_manifest.DEFAULT_MANIFEST)
    ap.add_argument("--fixtures", action="append", default=None,
                    help="glob(s) over fixture names; default: every "
                         "java_timeout board in the manifest")
    ap.add_argument("--force", action="store_true",
                    help="re-run boards that already have a log")
    ap.add_argument("--allow-unpinned-jar", action="store_true")
    args = ap.parse_args(argv)
    args.config = f"mp{args.passes}-{int(args.timeout)}s-uncapped-hermetic"

    pin = run_bench.verify_jar(args.jar, allow_unpinned=args.allow_unpinned_jar)
    if not pin["ok"]:
        print(pin["error"], file=sys.stderr)
        return 2

    if args.fixtures:
        names = sorted({
            p for g in args.fixtures
            for p in glob.glob(os.path.join(args.fixtures_dir, g))
            if os.path.isfile(p) and p.lower().endswith(".dsn")
        })
    else:
        man = fixture_manifest.load_manifest(args.manifest)
        if man is None:
            print(f"error: no manifest at {args.manifest}", file=sys.stderr)
            return 2
        names = [
            os.path.join(args.fixtures_dir, e["name"])
            for e in man["fixtures"]
            if e["java"]["class"] == "java_timeout"
        ]
    if not names:
        print("error: no boards to run", file=sys.stderr)
        return 2

    os.makedirs(args.out_dir, exist_ok=True)
    todo = [
        f for f in names
        if args.force or not os.path.isfile(os.path.join(
            args.out_dir, os.path.splitext(os.path.basename(f))[0] + ".log"))
    ]
    skipped = len(names) - len(todo)
    print(f"uncapped hermetic: {len(names)} boards ({skipped} already have a "
          f"log, --force to redo), W={args.workers}, cap={args.timeout}s, "
          f"passes={args.passes}", flush=True)

    meta = {
        "schema": run_bench.RESULTS_SCHEMA,
        "generated": _dt.datetime.now().isoformat(timespec="seconds"),
        "config_label": args.config,
        "tools": ["java"],
        "passes": args.passes,
        "timeout_s": args.timeout,
        "java": args.java,
        "jar": args.jar,
        "jar_pin": pin["meta"],
        "allow_unpinned_jar": args.allow_unpinned_jar,
        "fixtures_dir": args.fixtures_dir,
        "concurrency": args.workers,
        "contended": True,
        "serial_perf": False,
        "repo_rev": run_bench._git_rev(),
    }

    records: list[dict] = []
    prev_path = os.path.join(args.out_dir, "results.json")
    if os.path.isfile(prev_path):
        _pm, prev = fixture_manifest.load_results(prev_path)
        keep = {r["fixture"] for r in prev} - {
            os.path.basename(f) for f in todo}
        records.extend(r for r in prev if r["fixture"] in keep)
    done = 0
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as pool:
        futs = {pool.submit(run_board, f, args.out_dir, args): f for f in todo}
        for fut in concurrent.futures.as_completed(futs):
            rec = fut.result()
            with _write_lock:
                records.append(rec)
                records.sort(key=lambda r: r["fixture"])
                write_results(args.out_dir, records, meta)
            done += 1
            u = rec["uncapped"]
            print(f"[{done}/{len(todo)}] {rec['fixture']} "
                  f"status={u['status']} wall={rec['wall_s']} "
                  f"u/v={rec['unrouted']}/{rec['violations']} "
                  f"log={rec['log']}", flush=True)

    print(f"wrote {os.path.join(args.out_dir, 'results.json')} "
          f"({len(records)} records)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
