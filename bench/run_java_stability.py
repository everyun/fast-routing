#!/usr/bin/env python3
"""3-run Java stability sweep for the capped baseline (-mp 20, 300 s, W=4).

Writes:
  bench/results/java-stability/<stem>.r{1,2,3}.log
  bench/results/java-stability/<stem>.r{1,2,3}.ses
  bench/results/java-reference/ses/<stem>.java.ses   (round 1, G2.1 overlay)
  bench/results/java-reference/results.json          (round 1, for java.class)

Each Java invocation gets a fresh empty --user_data_path directory.

    python3 bench/run_java_stability.py
    python3 bench/run_java_stability.py --workers 4 --rounds 3
"""

from __future__ import annotations

import argparse
import concurrent.futures
import datetime as _dt
import glob
import json
import os
import shutil
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import manifest as fixture_manifest
import run_bench

BENCH_DIR = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.dirname(BENCH_DIR)
DEFAULT_STABILITY_DIR = os.path.join(BENCH_DIR, "results", "java-stability")
DEFAULT_REFERENCE_DIR = os.path.join(BENCH_DIR, "results", "java-reference")
DEFAULT_ARCHIVE = os.path.join(
    BENCH_DIR, "results", "java-reference-contaminated-20260829")
OLD_SES = os.path.join(BENCH_DIR, "results", "20260828-013025", "ses")


class _Args:
    """Minimal namespace run_bench.run_one reads."""

    def __init__(self, *, java, jar, passes, timeout, concurrency, config):
        self.java = java
        self.jar = jar
        self.passes = passes
        self.timeout = timeout
        self.concurrency = concurrency
        self.config = config
        self.rust_bin = run_bench.DEFAULT_RUST_BIN


def archive_old_reference(archive_dir: str) -> None:
    os.makedirs(archive_dir, exist_ok=True)
    note = os.path.join(archive_dir, "README.md")
    if not os.path.exists(note):
        with open(note, "w", encoding="utf-8") as f:
            f.write(
                "# Contaminated Java reference (pre-user_data_path isolation)\n\n"
                "Sessions and the 20260828-013025 results that G2.1 used before\n"
                "the `$TMPDIR/freerouting/freerouting.json` leak was closed.\n"
                "`.ses` files were gitignored; this archive keeps whatever was\n"
                "still on disk plus a pointer at the logs/results.json.\n"
            )
    src_results = os.path.join(
        BENCH_DIR, "results", "20260828-013025", "results.json")
    if os.path.isfile(src_results):
        shutil.copy2(src_results, os.path.join(archive_dir, "results.json"))
    if os.path.isdir(OLD_SES):
        dest_ses = os.path.join(archive_dir, "ses")
        os.makedirs(dest_ses, exist_ok=True)
        for path in glob.glob(os.path.join(OLD_SES, "*")):
            shutil.copy2(path, os.path.join(dest_ses, os.path.basename(path)))


def run_job(job: dict) -> dict:
    """One (fixture, round) Java run. Returns the bench record plus log path."""
    stem = os.path.splitext(os.path.basename(job["fixture"]))[0]
    round_i = job["round"]
    stability_dir = job["stability_dir"]
    os.makedirs(stability_dir, exist_ok=True)
    # Isolate this one run's ses/logs under a temp out_dir, then copy.
    out_dir = tempfile.mkdtemp(prefix=f"{stem}.r{round_i}.", dir=stability_dir)
    args = _Args(
        java=job["java"],
        jar=job["jar"],
        passes=job["passes"],
        timeout=job["timeout"],
        concurrency=job["workers"],
        config=job["config"],
    )
    rec = run_bench.run_one("java", job["fixture"], out_dir, args)
    log_path = os.path.join(stability_dir, f"{stem}.r{round_i}.log")
    stdout_path = os.path.join(out_dir, "logs", f"{stem}.java.stdout.txt")
    stderr_path = os.path.join(out_dir, "logs", f"{stem}.java.stderr.txt")
    parts = []
    for p in (stdout_path, stderr_path):
        if os.path.isfile(p):
            with open(p, encoding="utf-8", errors="replace") as f:
                parts.append(f.read())
    with open(log_path, "w", encoding="utf-8") as f:
        f.write("".join(parts))
    ses_src = rec.get("ses_path")
    ses_dst = os.path.join(stability_dir, f"{stem}.r{round_i}.ses")
    if ses_src and os.path.isfile(ses_src):
        shutil.copy2(ses_src, ses_dst)
        rec["stability_ses"] = ses_dst
    rec["stability_log"] = log_path
    rec["stability_round"] = round_i
    rec["stability_out_dir"] = out_dir
    return rec


def write_round1_reference(records: list[dict], reference_dir: str,
                           meta: dict) -> None:
    ses_dir = os.path.join(reference_dir, "ses")
    os.makedirs(ses_dir, exist_ok=True)
    r1 = [r for r in records if r.get("stability_round") == 1]
    for rec in r1:
        stem = os.path.splitext(rec["fixture"])[0]
        src = rec.get("stability_ses") or rec.get("ses_path")
        if src and os.path.isfile(src):
            shutil.copy2(src, os.path.join(ses_dir, f"{stem}.java.ses"))
    path = os.path.join(reference_dir, "results.json")
    with open(path, "w", encoding="utf-8") as f:
        json.dump({"meta": meta, "records": r1}, f, indent=2, ensure_ascii=False)
    print(f"wrote {path} ({len(r1)} round-1 records)")
    print(f"wrote {ses_dir} ({len(glob.glob(ses_dir + '/*.java.ses'))} sessions)")


def merge_uncapped(new_man: dict, old_path: str) -> dict:
    """Keep previously measured uncapped blocks when this sweep did not rerun them."""
    old = fixture_manifest.load_manifest(old_path)
    if not old:
        return new_man
    by_name = {e["name"]: e for e in old.get("fixtures", [])}
    for entry in new_man.get("fixtures", []):
        if entry.get("uncapped") is None and by_name.get(entry["name"], {}).get("uncapped"):
            entry["uncapped"] = by_name[entry["name"]]["uncapped"]
    if new_man.get("java_uncapped_source", {}).get("dir") is None:
        src = old.get("java_uncapped_source")
        if src:
            new_man["java_uncapped_source"] = src
    for key in ("uncapped_measured", "by_uncapped_status",
                "timeout_without_uncapped_wall"):
        if key in (old.get("coverage") or {}) and key in (new_man.get("coverage") or {}):
            if not new_man["coverage"].get("uncapped_measured"):
                new_man["coverage"][key] = old["coverage"][key]
    return new_man


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--fixtures-dir", default=run_bench.DEFAULT_FIXTURES_DIR)
    ap.add_argument("--jar", default=run_bench.DEFAULT_JAR)
    ap.add_argument("--java", default=run_bench.DEFAULT_JAVA)
    ap.add_argument("--passes", type=int, default=20)
    ap.add_argument("--timeout", type=float, default=300.0)
    ap.add_argument("--workers", "-W", type=int, default=4)
    ap.add_argument("--rounds", type=int, default=3)
    ap.add_argument("--stability-dir", default=DEFAULT_STABILITY_DIR)
    ap.add_argument("--reference-dir", default=DEFAULT_REFERENCE_DIR)
    ap.add_argument("--archive", default=DEFAULT_ARCHIVE)
    ap.add_argument("--allow-unpinned-jar", action="store_true")
    ap.add_argument("--fixtures", action="append", default=None)
    args = ap.parse_args(argv)

    pin = run_bench.verify_jar(args.jar, allow_unpinned=args.allow_unpinned_jar)
    if not pin["ok"]:
        print(pin["error"], file=sys.stderr)
        return 2

    globs = args.fixtures or ["*.dsn"]
    fixtures = sorted({
        p for g in globs
        for p in glob.glob(os.path.join(args.fixtures_dir, g))
        if os.path.isfile(p) and p.lower().endswith(".dsn")
    })
    if not fixtures:
        print(f"error: no fixtures in {args.fixtures_dir}", file=sys.stderr)
        return 2

    archive_old_reference(args.archive)
    os.makedirs(args.stability_dir, exist_ok=True)
    os.makedirs(args.reference_dir, exist_ok=True)

    jobs = []
    for fixture in fixtures:
        for round_i in range(1, args.rounds + 1):
            jobs.append({
                "fixture": fixture,
                "round": round_i,
                "java": args.java,
                "jar": args.jar,
                "passes": args.passes,
                "timeout": args.timeout,
                "workers": args.workers,
                "config": f"mp{args.passes}-{int(args.timeout)}s-stability",
                "stability_dir": args.stability_dir,
            })

    print(f"java stability: {len(fixtures)} fixtures × {args.rounds} rounds "
          f"= {len(jobs)} jobs, W={args.workers}, timeout={args.timeout}s, "
          f"passes={args.passes}")
    records = []
    done = 0
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as pool:
        futs = [pool.submit(run_job, job) for job in jobs]
        for fut in concurrent.futures.as_completed(futs):
            rec = fut.result()
            records.append(rec)
            done += 1
            status = ("TIMEOUT" if rec.get("timed_out")
                      else "CRASH" if rec.get("crashed")
                      else f"u/v={rec.get('unrouted')}/{rec.get('violations')}")
            print(f"[{done}/{len(jobs)}] r{rec['stability_round']} "
                  f"{rec['fixture']} {status} wall={rec.get('wall_s')}",
                  flush=True)

    records.sort(key=lambda r: (r.get("fixture"), r.get("stability_round")))
    meta = {
        "schema": run_bench.RESULTS_SCHEMA,
        "generated": _dt.datetime.now().isoformat(timespec="seconds"),
        "config_label": f"mp{args.passes}-{int(args.timeout)}s-stability",
        "tools": ["java"],
        "passes": args.passes,
        "timeout_s": args.timeout,
        "java": args.java,
        "jar": args.jar,
        "jar_pin": pin["meta"],
        "allow_unpinned_jar": args.allow_unpinned_jar,
        "fixtures_dir": args.fixtures_dir,
        "fixture_count": len(fixtures),
        "concurrency": args.workers,
        "serial_perf": False,
        "rounds": args.rounds,
        "repo_rev": run_bench._git_rev(),
    }
    sweep_path = os.path.join(args.stability_dir, "sweep.json")
    with open(sweep_path, "w", encoding="utf-8") as f:
        json.dump({"meta": meta, "records": records}, f, indent=2,
                  ensure_ascii=False)
    print(f"wrote {sweep_path}")

    write_round1_reference(records, args.reference_dir, meta)

    results_path = os.path.join(args.reference_dir, "results.json")
    man = fixture_manifest.build_manifest(
        args.fixtures_dir, results_path, None, args.stability_dir)
    man = merge_uncapped(man, fixture_manifest.DEFAULT_MANIFEST)
    out_man = fixture_manifest.DEFAULT_MANIFEST
    with open(out_man, "w", encoding="utf-8") as f:
        f.write(fixture_manifest.dumps(man))
    print(f"wrote {out_man}")
    cov = man["coverage"]
    print("  java classes: " + ", ".join(
        f"{k}={v}" for k, v in cov["by_java_class"].items() if v))
    print("  stability: " + ", ".join(
        f"{k}={v}" for k, v in cov["by_stability_class"].items() if v))
    return 0


if __name__ == "__main__":
    sys.exit(main())
