#!/usr/bin/env python3
"""Runner script executed on orca for quality sweeps (24 / 46 / OOS).

Executes the suite with W concurrent workers, records wall times and per-board
logs, extracts sweep.json, runs g21_judge.py to evaluate against reference,
and writes out summary.txt and gate.json.
"""
from __future__ import annotations

import argparse
import json
import os
import signal
import subprocess
import sys
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


def run_one(
    bin_path: Path,
    fixtures_dir: Path,
    ses_dir: Path,
    log_dir: Path,
    board: str,
    mp: int,
    job_timeout: int,
    hard_timeout: int,
    doors: str,
) -> dict:
    dsn = fixtures_dir / f"{board}.dsn"
    ses = ses_dir / f"{board}.rust.ses"
    log = log_dir / f"{board}.log"

    env = dict(os.environ)
    if doors in ("doors", "on"):
        env["FR_ROUTER_TILING_45"] = "doors"
    elif doors == "full":
        env["FR_ROUTER_TILING_45"] = "full"
    elif doors == "off":
        env["FR_ROUTER_TILING_45"] = "off"
    elif doors == "default":
        # Do not override FR_ROUTER_TILING_45, use binary default or ambient env
        env.pop("FR_ROUTER_TILING_45", None)

    cmd = [
        "nice", "-n", "10",
        str(bin_path),
        "-de", str(dsn),
        "-do", str(ses),
        "-mp", str(mp),
        "--fanout",
        "--job-timeout", str(job_timeout),
    ]

    t0 = time.monotonic()
    with log.open("w") as fh:
        proc = subprocess.Popen(
            cmd,
            stdout=fh,
            stderr=subprocess.STDOUT,
            start_new_session=True,
            env=env,
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
    }


def build_sweep(log_dir: Path, summary: Path, out: Path, doors_mode: str = "default") -> None:
    rc_by_board = {}
    for line in summary.read_text(errors="replace").splitlines():
        parts = line.split()
        if len(parts) >= 2 and parts[1].startswith("rc="):
            try:
                rc_by_board[parts[0]] = int(parts[1][3:])
            except ValueError:
                pass

    records = []
    for log in sorted(log_dir.glob("*.log")):
        board = log.stem
        text = log.read_text(errors="replace")
        referee = next(
            (line for line in text.splitlines() if line.startswith("Referee: ")),
            None,
        )
        stop = next(
            (
                line.split("Router stopped on ", 1)[1]
                for line in text.splitlines()
                if line.startswith("Router stopped on ")
            ),
            None,
        )
        records.append(
            {
                "fixture": f"{board}.dsn",
                "wall_s": None,
                "timed_out": rc_by_board.get(board) == 124,
                "stop_reason": stop,
                "doors_mode": doors_mode,
                "self_referee": referee,
            }
        )
    out.write_text(json.dumps(records, indent=1, ensure_ascii=False) + "\n")


def main() -> int:
    ap = argparse.ArgumentParser(description="Remote quality suite runner")
    ap.add_argument("--bin", required=True, help="Path to fr-cli binary")
    ap.add_argument("--referee", required=True, help="Path to g21-referee binary")
    ap.add_argument("--suite", choices=["24", "46", "oos"], default="46")
    ap.add_argument("--mp", type=int, default=20)
    ap.add_argument("--job-timeout", type=int, default=285)
    ap.add_argument("--hard-timeout", type=int, default=None)
    ap.add_argument("--doors", choices=["default", "full", "doors", "on", "off"], default="default")
    ap.add_argument("-w", "--concurrency", type=int, default=8)
    ap.add_argument("--out", required=True, help="Output directory")
    ap.add_argument("--repo-root", default=str(Path(__file__).resolve().parents[3]))
    ap.add_argument("--fixtures-dir", default=None)
    ap.add_argument("--manifest", default=None)
    ap.add_argument("--java-ses", default=None)

    args = ap.parse_args()
    repo_root = Path(args.repo_root).resolve()
    bin_path = Path(args.bin).resolve()
    referee_path = Path(args.referee).resolve()
    out_dir = Path(args.out).resolve()
    hard_timeout = args.hard_timeout or (args.job_timeout + 35)

    if args.suite == "oos":
        fixtures_dir = Path(args.fixtures_dir or (repo_root / "bench/fixtures-oos")).resolve()
        manifest = Path(args.manifest or (fixtures_dir / "manifest.json")).resolve()
        java_ses = Path(args.java_ses or (repo_root / "bench/results/oos-java-reference/ses")).resolve()
        boards = [b for b in SUITE_OOS if (fixtures_dir / f"{b}.dsn").is_file()]
    elif args.suite == "24":
        fixtures_dir = Path(args.fixtures_dir or "/tmp/freerouting-upstream/fixtures").resolve()
        manifest = Path(args.manifest or (repo_root / "bench/fixtures-manifest.json")).resolve()
        java_ses = Path(args.java_ses or (repo_root / "bench/results/java-reference/ses")).resolve()
        boards = [b for b in SUITE_24 if (fixtures_dir / f"{b}.dsn").is_file()]
    else:  # 46
        fixtures_dir = Path(args.fixtures_dir or "/tmp/freerouting-upstream/fixtures").resolve()
        manifest = Path(args.manifest or (repo_root / "bench/fixtures-manifest.json")).resolve()
        java_ses = Path(args.java_ses or (repo_root / "bench/results/java-reference/ses")).resolve()
        manifest_data = json.loads(manifest.read_text(encoding="utf-8"))
        boards = sorted(
            Path(f["name"]).stem
            for f in manifest_data["fixtures"]
            if (fixtures_dir / f["name"]).is_file()
        )

    ses_dir = out_dir / "ses"
    log_dir = out_dir / "logs"
    ses_dir.mkdir(parents=True, exist_ok=True)
    log_dir.mkdir(parents=True, exist_ok=True)

    summary_file = out_dir / "summary.txt"
    sweep_file = out_dir / "sweep.json"
    gate_file = out_dir / "gate.json"

    effective_doors = args.doors
    if args.doors in ("doors", "on"):
        effective_doors = "doors"
    elif args.doors == "full":
        effective_doors = "full"
    elif args.doors == "off":
        effective_doors = "off"
    elif args.doors == "default":
        ambient = os.environ.get("FR_ROUTER_TILING_45")
        if ambient:
            effective_doors = f"default({ambient})"
        else:
            effective_doors = "default"

    print(f"==> Suite: {args.suite} ({len(boards)} boards) W={args.concurrency} mp={args.mp} "
          f"job_timeout={args.job_timeout}s doors={effective_doors}")
    print(f"    bin:       {bin_path}")
    print(f"    referee:   {referee_path}")
    print(f"    fixtures:  {fixtures_dir}")
    print(f"    out:       {out_dir}")

    t_start = time.monotonic()
    with summary_file.open("w") as s:
        s.write(
            f"SUITE_START {utc()} suite={args.suite} W={args.concurrency} mp={args.mp} "
            f"job_timeout={args.job_timeout} hard_timeout={hard_timeout} doors={effective_doors} "
            f"bin={bin_path} n={len(boards)}\n"
        )
        s.flush()

        completed = 0
        with ThreadPoolExecutor(max_workers=args.concurrency) as pool:
            futs = {
                pool.submit(
                    run_one,
                    bin_path,
                    fixtures_dir,
                    ses_dir,
                    log_dir,
                    b,
                    args.mp,
                    args.job_timeout,
                    hard_timeout,
                    args.doors,
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
        s.write(f"SUITE_END {utc()} n={len(boards)} total_wall_s={total_wall}\n")

    build_sweep(log_dir, summary_file, sweep_file, doors_mode=effective_doors)
    print(f"==> Boards done in {total_wall}s; running g21_judge.py...")

    judge_script = repo_root / "bench/tools/g21_judge.py"
    judge_cmd = [
        sys.executable,
        str(judge_script),
        "--ours", str(ses_dir),
        "--referee", str(referee_path),
        "--manifest", str(manifest),
        "--fixtures", str(fixtures_dir),
        "--sweep", str(sweep_file),
        "--out", str(gate_file),
    ]
    if java_ses.is_dir():
        judge_cmd.extend(["--java", str(java_ses)])

    judge_res = subprocess.run(judge_cmd, text=True, capture_output=True)
    (out_dir / "judge.stdout.txt").write_text(judge_res.stdout)
    if judge_res.stderr:
        (out_dir / "judge.stderr.txt").write_text(judge_res.stderr)

    print("\n" + judge_res.stdout)
    if judge_res.returncode != 0:
        print(f"g21_judge stderr:\n{judge_res.stderr}", file=sys.stderr)

    return judge_res.returncode


if __name__ == "__main__":
    raise SystemExit(main())
