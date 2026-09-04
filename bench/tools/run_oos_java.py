#!/usr/bin/env python3
"""3-round Java reference for the G2.4 out-of-sample suite.

Same config as the main suite: -mp 20, 300 s, English logs, headless.
Each run gets a fresh empty --user_data_path (COMMON.md: without it the jar
reads $TMPDIR/freerouting/freerouting.json and can disable fanout/optimizer).

Quality-only: concurrency is declared (default 2) and timings are contended.

Usage (from the repo root):

    python3 bench/tools/run_oos_java.py
    python3 bench/tools/run_oos_java.py --rounds 3 --timeout 300 --passes 20
"""

from __future__ import annotations

import argparse
import datetime as _dt
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from concurrent.futures import ThreadPoolExecutor, as_completed

BENCH_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REPO_ROOT = os.path.dirname(BENCH_DIR)
sys.path.insert(0, BENCH_DIR)
import check_ses  # noqa: E402
import manifest as fixture_manifest  # noqa: E402
import run_bench  # noqa: E402

DEFAULT_FIXTURES = os.path.join(BENCH_DIR, "fixtures-oos")
DEFAULT_OUT = os.path.join(BENCH_DIR, "results", "oos-java-reference")
DEFAULT_JAR = "/tmp/frbase/freerouting-2.3.0.jar"
FALLBACK_JAR = os.path.expanduser("~/fr-ref/frbase/freerouting-2.3.0.jar")
DEFAULT_JAVA = "/opt/homebrew/opt/openjdk/bin/java"
TIME_BIN = "/usr/bin/time"


def resolve_jar(explicit: str | None) -> str:
    for cand in (explicit, DEFAULT_JAR, FALLBACK_JAR):
        if cand and os.path.isfile(cand):
            return cand
    raise SystemExit(
        "error: no freerouting jar at /tmp/frbase or ~/fr-ref/frbase "
        "(wait for INFRA_READY: yes)"
    )


def one_run(java, jar, fixture, ses_path, log_path, timeout, passes) -> dict:
    user_data = tempfile.mkdtemp(prefix="fr-userdata-")
    cmd = [
        TIME_BIN, "-l",
        "nice", "-n", "10",
        java, "-Djava.awt.headless=true",
        "-jar", jar,
        "-de", fixture, "-do", ses_path,
        "-mp", str(passes), "-l", "en",
        f"--user_data_path={user_data}",
    ]
    t0 = time.monotonic()
    proc = subprocess.Popen(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        errors="replace",
        start_new_session=True,
    )
    timed_out = False
    try:
        stdout, stderr = proc.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        timed_out = True
        try:
            os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass
        stdout, stderr = proc.communicate()
    wall = round(time.monotonic() - t0, 3)
    shutil.rmtree(user_data, ignore_errors=True)
    with open(log_path, "w", encoding="utf-8") as f:
        f.write(stdout)
        f.write("\n----- stderr /usr/bin/time -----\n")
        f.write(stderr)
    rec = {
        "fixture": os.path.basename(fixture),
        "cmd": cmd,
        "timed_out": timed_out,
        "exit_code": proc.returncode,
        "wall_s": wall,
        "ses_path": ses_path if os.path.exists(ses_path) else None,
        "ses_bytes": os.path.getsize(ses_path) if os.path.exists(ses_path) else 0,
        "log": log_path,
    }
    rec.update(run_bench.parse_time_footer(stderr))
    rec.update(run_bench.parse_java_log(stdout))
    rec["crashed"] = (not timed_out) and (
        proc.returncode not in (0,) or not rec.get("parse_ok")
    )
    if rec["ses_path"]:
        try:
            rec["check"] = check_ses.check(fixture, ses_path)
            rec["check"].pop("per_net", None)
        except Exception as e:  # noqa: BLE001
            rec["check_error"] = f"{type(e).__name__}: {e}"
    return rec


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--fixtures-dir", default=DEFAULT_FIXTURES)
    ap.add_argument("--out", default=DEFAULT_OUT)
    ap.add_argument("--jar", default=None)
    ap.add_argument("--java", default=DEFAULT_JAVA)
    ap.add_argument("--passes", type=int, default=20)
    ap.add_argument("--timeout", type=float, default=300.0)
    ap.add_argument("--rounds", type=int, default=3)
    ap.add_argument("--concurrency", type=int, default=2)
    args = ap.parse_args(argv)

    jar = resolve_jar(args.jar)
    fixtures = sorted(
        os.path.join(args.fixtures_dir, n)
        for n in os.listdir(args.fixtures_dir)
        if n.endswith(".dsn")
    )
    if not fixtures:
        raise SystemExit(f"error: no *.dsn in {args.fixtures_dir}")
    os.makedirs(args.out, exist_ok=True)
    logs_dir = os.path.join(args.out, "logs")
    ses_dir = os.path.join(args.out, "ses")
    os.makedirs(logs_dir, exist_ok=True)
    os.makedirs(ses_dir, exist_ok=True)

    jobs = []
    for rnd in range(1, args.rounds + 1):
        for fx in fixtures:
            stem = os.path.basename(fx)[: -len(".dsn")]
            ses = os.path.join(ses_dir, f"{stem}.r{rnd}.java.ses")
            log = os.path.join(logs_dir, f"{stem}.r{rnd}.log")
            jobs.append((rnd, fx, ses, log))

    print(
        f"java reference: {len(fixtures)} boards × {args.rounds} rounds = "
        f"{len(jobs)} runs, W={args.concurrency}, jar={jar}",
        flush=True,
    )
    records = []
    with ThreadPoolExecutor(max_workers=args.concurrency) as pool:
        futs = {
            pool.submit(
                one_run, args.java, jar, fx, ses, log, args.timeout, args.passes
            ): (rnd, fx)
            for rnd, fx, ses, log in jobs
        }
        for fut in as_completed(futs):
            rnd, fx = futs[fut]
            rec = fut.result()
            rec["round"] = rnd
            rec["concurrency"] = args.concurrency
            rec["tool"] = "java"
            records.append(rec)
            flag = "TIMEOUT" if rec["timed_out"] else (
                "CRASH" if rec["crashed"] else f"{rec.get('unrouted')}/{rec.get('violations')}"
            )
            print(
                f"  r{rnd} {os.path.basename(fx):40s} {flag:12s} {rec['wall_s']:.1f}s",
                flush=True,
            )

    # Canonical reference session = round 1 (stability class is computed from all 3).
    for fx in fixtures:
        stem = os.path.basename(fx)[: -len(".dsn")]
        src = os.path.join(ses_dir, f"{stem}.r1.java.ses")
        dst = os.path.join(ses_dir, f"{stem}.java.ses")
        if os.path.exists(src) and os.path.getsize(src) > 0:
            shutil.copy2(src, dst)

    # Fold logs into the stability layout manifest.py already knows:
    #   <stability-dir>/<stem>.r<n>.log
    stability_dir = os.path.join(args.out, "stability")
    os.makedirs(stability_dir, exist_ok=True)
    for rec in records:
        stem = rec["fixture"][: -len(".dsn")]
        shutil.copy2(rec["log"], os.path.join(stability_dir, f"{stem}.r{rec['round']}.log"))

    meta = {
        "generated": _dt.datetime.now(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "config_label": "oos-mp20-300s",
        "passes": args.passes,
        "timeout_s": args.timeout,
        "rounds": args.rounds,
        "concurrency": args.concurrency,
        "jar": jar,
        "java": args.java,
        "fixtures_dir": args.fixtures_dir,
        "note": "contended quality sweep; do not quote wall times",
        "repo_rev": subprocess.run(
            ["git", "-C", REPO_ROOT, "rev-parse", "--short", "HEAD"],
            capture_output=True, text=True,
        ).stdout.strip(),
    }
    results_path = os.path.join(args.out, "results.json")
    with open(results_path, "w", encoding="utf-8") as f:
        json.dump({"meta": meta, "records": records}, f, indent=1, ensure_ascii=False)
    print(f"wrote {results_path}")

    # Rebuild the oos manifest with java.class + stability.
    man = fixture_manifest.build_manifest(
        args.fixtures_dir,
        results_path,
        uncapped_dir=None,
        stability_dir=stability_dir,
    )
    sources_path = os.path.join(args.fixtures_dir, "sources.json")
    if os.path.exists(sources_path):
        with open(sources_path, "r", encoding="utf-8") as f:
            sources = json.load(f)
        for entry in man["fixtures"]:
            stem = entry["name"][: -len(".dsn")]
            if stem in sources:
                entry["source"] = sources[stem]
    man_path = os.path.join(args.fixtures_dir, "manifest.json")
    with open(man_path, "w", encoding="utf-8") as f:
        f.write(fixture_manifest.dumps(man))
    print(f"wrote {man_path}")
    shutil.copy2(man_path, os.path.join(args.out, "manifest.json"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
