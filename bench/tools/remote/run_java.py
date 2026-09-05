#!/usr/bin/env python3
"""Java reference runner executed on orca for 24 / 46 / OOS suites.

Runs the pinned freerouting-2.3.0.jar with isolated --user_data_path per run
under W concurrent workers.
"""
from __future__ import annotations

import argparse
import json
import os
import signal
import subprocess
import sys
import tempfile
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime, timezone
from pathlib import Path

SUITE_24 = [
    "Issue026-J2_reference",
    "Issue034-Green14SegLED",
    "Issue035-ReadPlaceScope",
    "Issue039-bug-design",
    "Issue054-tairakb",
    "Issue093-interf_u",
    "Issue102-Mars-64-revE-rot00",
    "Issue103-Board-Routed",
    "Issue107-freq_teiler_200kHz_kicad",
    "Issue110-Pajalnaja_stancija",
    "Issue110-RelayModule",
    "Issue110-testPCBSpecctraFile",
    "Issue110-testProjectFromFreeroutingBugTest",
    "Issue143-rpi_splitter",
    "Issue153-wavefolder",
    "Issue155-CH376_MCP795_Module",
    "Issue157-TeamAdapt-LinePCB",
    "Issue159-setonix_2hp-pcb",
    "Issue163-pic_programmer",
    "Issue187-processor.Z80",
    "Issue209-split10",
    "Issue217-8088sbc",
    "Issue230-CNH_Functional_Tester_1",
    "Issue269-caniot-tiny-arm",
]

SUITE_OOS = [
    "cla45-compute-hdi6cs",
    "cla45-computeboard",
    "cla45-power-io-l6",
    "cla45-sensor-hdi",
    "cla45-sensor-l6",
    "easyeda-pcb1",
    "kicad-coldfire-unrouted",
    "kicad-coldfire",
    "kicad-tutorial",
]


def utc() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def run_one_java(
    jar_path: Path,
    fixtures_dir: Path,
    ses_dir: Path,
    log_dir: Path,
    user_data_root: Path,
    board: str,
    mp: int,
    job_timeout: int,
    hard_timeout: int,
) -> dict:
    dsn = fixtures_dir / f"{board}.dsn"
    ses = ses_dir / f"{board}.java.ses"
    log = log_dir / f"{board}.log"
    user_data_dir = tempfile.mkdtemp(prefix=f"{board}.", dir=str(user_data_root))

    cmd = [
        "nice", "-n", "10",
        "java",
        "-Djava.awt.headless=true",
        "-jar", str(jar_path),
        "-de", str(dsn),
        "-do", str(ses),
        "-mp", str(mp),
        "-l", "en",
        f"--user_data_path={user_data_dir}",
    ]

    t0 = time.monotonic()
    with log.open("w") as fh:
        proc = subprocess.Popen(
            cmd,
            stdout=fh,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        try:
            rc = proc.wait(timeout=hard_timeout)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(proc.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                proc.wait()
            rc = 124
    wall = time.monotonic() - t0
    return {
        "board": board,
        "rc": rc,
        "wall": round(wall, 2),
        "ses_bytes": ses.stat().st_size if ses.exists() else 0,
        "user_data_dir": user_data_dir,
    }


def main() -> int:
    ap = argparse.ArgumentParser(description="Remote Java reference runner")
    ap.add_argument("--jar", default="/tmp/frbase/freerouting-2.3.0.jar")
    ap.add_argument("--suite", choices=["24", "46", "oos"], default="46")
    ap.add_argument("--mp", type=int, default=20)
    ap.add_argument("--job-timeout", type=int, default=285)
    ap.add_argument("--hard-timeout", type=int, default=None)
    ap.add_argument("-w", "--concurrency", type=int, default=8)
    ap.add_argument("--out", required=True, help="Output directory")
    ap.add_argument("--repo-root", default=None)
    ap.add_argument("--fixtures-dir", default=None)

    args = ap.parse_args()
    jar_path = Path(args.jar).resolve()
    out_dir = Path(args.out).resolve()
    hard_timeout = args.hard_timeout or (args.job_timeout + 35)

    repo_root = None
    if args.repo_root:
        repo_root = Path(args.repo_root).resolve()
    else:
        for p in sorted(Path.home().glob("fr/worktrees/*")):
            if (p / "bench/fixtures-oos").is_dir():
                repo_root = p
                break

    if args.suite == "oos":
        if args.fixtures_dir:
            fixtures_dir = Path(args.fixtures_dir).resolve()
        elif repo_root and (repo_root / "bench/fixtures-oos").is_dir():
            fixtures_dir = repo_root / "bench/fixtures-oos"
        else:
            fixtures_dir = Path("/tmp/freerouting-upstream/fixtures-oos").resolve()
        boards = [b for b in SUITE_OOS if (fixtures_dir / f"{b}.dsn").is_file()]
    elif args.suite == "24":
        fixtures_dir = Path(args.fixtures_dir or "/tmp/freerouting-upstream/fixtures").resolve()
        boards = [b for b in SUITE_24 if (fixtures_dir / f"{b}.dsn").is_file()]
    else:  # 46
        fixtures_dir = Path(args.fixtures_dir or "/tmp/freerouting-upstream/fixtures").resolve()
        boards = sorted(p.stem for p in fixtures_dir.glob("*.dsn"))

    ses_dir = out_dir / "ses"
    log_dir = out_dir / "logs"
    user_data_root = out_dir / "java-user-data"
    ses_dir.mkdir(parents=True, exist_ok=True)
    log_dir.mkdir(parents=True, exist_ok=True)
    user_data_root.mkdir(parents=True, exist_ok=True)

    summary_file = out_dir / "summary.txt"

    print(f"==> Java Suite: {args.suite} ({len(boards)} boards) W={args.concurrency} mp={args.mp} "
          f"job_timeout={args.job_timeout}s")
    print(f"    jar:       {jar_path}")
    print(f"    fixtures:  {fixtures_dir}")
    print(f"    out:       {out_dir}")

    t_start = time.monotonic()
    with summary_file.open("w") as s:
        s.write(
            f"JAVA_SUITE_START {utc()} suite={args.suite} W={args.concurrency} mp={args.mp} "
            f"job_timeout={args.job_timeout} hard_timeout={hard_timeout} jar={jar_path} n={len(boards)}\n"
        )
        s.flush()

        completed = 0
        with ThreadPoolExecutor(max_workers=args.concurrency) as pool:
            futs = {
                pool.submit(
                    run_one_java,
                    jar_path,
                    fixtures_dir,
                    ses_dir,
                    log_dir,
                    user_data_root,
                    b,
                    args.mp,
                    args.job_timeout,
                    hard_timeout,
                ): b
                for b in boards
            }
            for fut in as_completed(futs):
                rec = fut.result()
                completed += 1
                status = "OK" if rec["rc"] == 0 else f"rc={rec['rc']}"
                print(f"[{completed:2d}/{len(boards)}] {rec['board']:<44} {status:<8} "
                      f"{rec['wall']:>6.1f}s ({rec['ses_bytes']} bytes ses)")
                s.write(
                    f"{rec['board']} rc={rec['rc']} wall={rec['wall']} "
                    f"ses_bytes={rec['ses_bytes']}\n"
                )
                s.flush()

        total_wall = round(time.monotonic() - t_start, 1)
        s.write(f"JAVA_SUITE_END {utc()} n={len(boards)} total_wall_s={total_wall}\n")

    print(f"==> Java reference run finished in {total_wall}s -> {out_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
