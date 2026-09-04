#!/usr/bin/env python3
"""ROADMAP G4: KiCad product gate.

For every out-of-sample (or any) board:

  * count SES wire-segment angle violations against the board's snap_angle
    (`none` / `ninety_degree` / `fortyfive_degree`, defaulting to 45° when
    the DSN declares nothing — same default as the Specctra reader);

and for every board that has a `.kicad_pcb` source:

  * copy the PCB, run `kicad-cli pcb drc` before SES import,
    `pcbnew.ImportSpecctraSES` the session into the copy, DRC again,
    report unconnected-item delta and DRC-violation delta.

The G4 4-tuple is (unrouted, added violations, angle violations, KiCad-DRC
delta). The first two numbers come from a G2.1 `gate.json` when one is
passed; they are `null` otherwise. This file never talks to the router.

Stdlib Python 3 only, like the rest of `bench/`. pcbnew / kicad-cli are
optional at import time so the unit tests stay hermetic.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import shutil
import subprocess
import sys
import tempfile

BENCH_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REPO_ROOT = os.path.dirname(BENCH_DIR)
sys.path.insert(0, BENCH_DIR)
import check_ses  # noqa: E402

KICAD_APP = os.path.expanduser("~/Applications/KiCad/KiCad.app")
DEFAULT_KICAD_CLI = os.path.join(KICAD_APP, "Contents/MacOS/kicad-cli")
DEFAULT_KICAD_PY = os.path.join(
    KICAD_APP,
    "Contents/Frameworks/Python.framework/Versions/3.9/bin/python3",
)

DEFAULT_SNAP = "fortyfive_degree"
ANGLE_EPS = 1e-6  # relative to segment length


# --------------------------------------------------------------------------
# snap_angle of a session
# --------------------------------------------------------------------------


def snap_angle_of(dsn_path: str) -> str:
    """Declared snap_angle, or the reader default."""
    with open(dsn_path, "r", encoding="utf-8", errors="replace") as f:
        tree = check_ses.parse_sexp(f.read())
    for structure in check_ses.iter_nodes(tree, "structure"):
        for snap in check_ses.child_nodes(structure, "snap_angle"):
            declared = check_ses.first_atom(snap)
            if declared:
                return declared.lower()
    return DEFAULT_SNAP


def _nums_after_width(path_node) -> list[float]:
    """(path <layer> <width> x1 y1 x2 y2 ...) -> [x1, y1, x2, y2, ...]."""
    nums = []
    for el in path_node[1:]:
        if isinstance(el, str):
            try:
                nums.append(float(el))
            except ValueError:
                pass
    return nums[1:]  # drop width


def segment_ok(dx: float, dy: float, snap: str, eps: float = ANGLE_EPS) -> bool:
    """True iff this displacement is legal under `snap`."""
    if dx == 0.0 and dy == 0.0:
        return True
    snap = (snap or DEFAULT_SNAP).lower()
    if snap in ("none",):
        return True
    length = math.hypot(dx, dy)
    axis = abs(dx) <= eps * length or abs(dy) <= eps * length
    if snap in ("ninety_degree", "ninety", "90"):
        return axis
    # fortyfive_degree (reader default) and anything we don't recognise.
    diag = abs(abs(dx) - abs(dy)) <= eps * length
    return axis or diag


def angle_violations(ses_path: str, snap: str) -> dict:
    """Walk every SES wire path; count illegal segments.

    A zero-length hop is ignored. Returns the count plus a few examples so a
    failing board is diagnosable without re-parsing the file.
    """
    with open(ses_path, "r", encoding="utf-8", errors="replace") as f:
        tree = check_ses.parse_sexp(f.read())
    bad = 0
    total = 0
    examples = []
    for wire in check_ses.iter_nodes(tree, "wire"):
        for path in check_ses.iter_nodes(wire, "path"):
            pts = _nums_after_width(path)
            for k in range(2, len(pts) - 1, 2):
                dx = pts[k] - pts[k - 2]
                dy = pts[k + 1] - pts[k - 1]
                if dx == 0.0 and dy == 0.0:
                    continue
                total += 1
                if not segment_ok(dx, dy, snap):
                    bad += 1
                    if len(examples) < 8:
                        examples.append(
                            {
                                "from": [pts[k - 2], pts[k - 1]],
                                "to": [pts[k], pts[k + 1]],
                                "dx": dx,
                                "dy": dy,
                            }
                        )
    return {
        "snap_angle": snap,
        "segments": total,
        "angle_violations": bad,
        "examples": examples,
    }


# --------------------------------------------------------------------------
# KiCad DRC JSON
# --------------------------------------------------------------------------


def count_drc(report: dict) -> dict:
    """Unconnected-item count and DRC-violation count from a KiCad JSON report.

    KiCad 8–10 put connectivity failures in `unconnected_items` and everything
    else in `violations`. Some builds also emit an `unconnected_items` type
    inside `violations`; those are counted as unconnected, not as DRC.
    """
    unconnected = list(report.get("unconnected_items") or [])
    violations = []
    for v in report.get("violations") or []:
        vtype = (v.get("type") or v.get("error_type") or "").lower()
        if vtype in ("unconnected_items", "unconnected_item", "unconnected"):
            unconnected.append(v)
        else:
            violations.append(v)
    return {
        "unconnected": len(unconnected),
        "violations": len(violations),
        "kicad_version": report.get("kicad_version"),
        "source": report.get("source"),
    }


def drc_delta(before: dict, after: dict) -> dict:
    """after − before for both counters."""
    b = count_drc(before)
    a = count_drc(after)
    return {
        "unconnected_before": b["unconnected"],
        "unconnected_after": a["unconnected"],
        "unconnected_delta": a["unconnected"] - b["unconnected"],
        "violations_before": b["violations"],
        "violations_after": a["violations"],
        "drc_delta": a["violations"] - b["violations"],
    }


# --------------------------------------------------------------------------
# pcbnew SES import + kicad-cli DRC (skipped in unit tests)
# --------------------------------------------------------------------------


def run_drc(kicad_cli: str, pcb_path: str, out_json: str) -> dict:
    cmd = [
        kicad_cli, "pcb", "drc",
        "--format", "json",
        "--severity-error",
        "-o", out_json,
        pcb_path,
    ]
    done = subprocess.run(cmd, capture_output=True, text=True, errors="replace")
    if not os.path.exists(out_json):
        raise RuntimeError(
            f"kicad-cli pcb drc produced no report (exit {done.returncode}): "
            f"{done.stderr[-400:]}"
        )
    with open(out_json, "r", encoding="utf-8") as f:
        return json.load(f)


def import_ses(kicad_py: str, pcb_path: str, ses_path: str, out_pcb: str) -> None:
    script = (
        "import wx\n"
        "app = wx.App(False)\n"
        "import pcbnew\n"
        f"board = pcbnew.LoadBoard({pcb_path!r})\n"
        f"ok = pcbnew.ImportSpecctraSES(board, {ses_path!r})\n"
        "if not ok:\n"
        "    raise SystemExit('ImportSpecctraSES returned False')\n"
        f"pcbnew.SaveBoard({out_pcb!r}, board)\n"
    )
    done = subprocess.run(
        [kicad_py, "-c", script],
        capture_output=True, text=True, errors="replace",
    )
    if done.returncode != 0:
        raise RuntimeError(
            f"ImportSpecctraSES failed (exit {done.returncode}): "
            f"{(done.stderr or done.stdout)[-600:]}"
        )


def kicad_roundtrip(
    pcb_path: str,
    ses_path: str,
    work_dir: str,
    kicad_cli: str,
    kicad_py: str,
) -> dict:
    """DRC before, import SES, DRC after. All artefacts stay in `work_dir`."""
    os.makedirs(work_dir, exist_ok=True)
    before_pcb = os.path.join(work_dir, "before.kicad_pcb")
    after_pcb = os.path.join(work_dir, "after.kicad_pcb")
    shutil.copy2(pcb_path, before_pcb)
    before_json = os.path.join(work_dir, "drc-before.json")
    after_json = os.path.join(work_dir, "drc-after.json")
    before = run_drc(kicad_cli, before_pcb, before_json)
    import_ses(kicad_py, before_pcb, ses_path, after_pcb)
    after = run_drc(kicad_cli, after_pcb, after_json)
    delta = drc_delta(before, after)
    delta["drc_before_path"] = before_json
    delta["drc_after_path"] = after_json
    delta["imported_pcb"] = after_pcb
    return delta


# --------------------------------------------------------------------------
# 4-tuple row
# --------------------------------------------------------------------------


def four_tuple(unrouted, added_violations, angle_violations_n, drc_delta_n) -> tuple:
    return (unrouted, added_violations, angle_violations_n, drc_delta_n)


def row_for(
    name: str,
    dsn: str,
    ses: str | None,
    pcb: str | None,
    gate_row: dict | None,
    work_root: str | None,
    kicad_cli: str,
    kicad_py: str,
    skip_kicad: bool,
) -> dict:
    snap = snap_angle_of(dsn) if os.path.exists(dsn) else DEFAULT_SNAP
    angles = (
        angle_violations(ses, snap)
        if ses and os.path.exists(ses) and os.path.getsize(ses) > 0
        else {"snap_angle": snap, "segments": 0, "angle_violations": None, "examples": []}
    )
    kicad = None
    if pcb and not skip_kicad and ses and os.path.exists(ses) and os.path.getsize(ses) > 0:
        work = os.path.join(work_root or tempfile.mkdtemp(prefix="g4-"), name)
        try:
            kicad = kicad_roundtrip(pcb, ses, work, kicad_cli, kicad_py)
        except Exception as e:  # noqa: BLE001 — one board must not kill the suite
            kicad = {"error": f"{type(e).__name__}: {e}"}
    elif pcb and skip_kicad:
        kicad = {"skipped": True}
    elif not pcb:
        kicad = {"skipped": True, "reason": "no kicad_pcb source"}

    unrouted = (gate_row or {}).get("ours_unrouted")
    added = (gate_row or {}).get("added_violations")
    angle_n = angles.get("angle_violations")
    drc_n = None if not kicad or "error" in kicad or kicad.get("skipped") else kicad["drc_delta"]
    return {
        "fixture": name,
        "tuple": four_tuple(unrouted, added, angle_n, drc_n),
        "unrouted": unrouted,
        "added_violations": added,
        "angle_violations": angle_n,
        "kicad_drc_delta": drc_n,
        "unconnected_delta": None if not kicad or "error" in kicad or kicad.get("skipped")
        else kicad["unconnected_delta"],
        "snap_angle": snap,
        "angles": angles,
        "kicad": kicad,
        "ses": ses,
        "pcb": pcb,
        "gate": (gate_row or {}).get("verdict"),
    }


def report(rows, stream=sys.stdout):
    print(
        f"{'fixture':36s} {'tuple (u, +v, ang, drcΔ)':28s} "
        f"{'unconnΔ':>8s} snap",
        file=stream,
    )
    for row in rows:
        t = row["tuple"]
        print(
            f"{row['fixture'][:34]:36s} {str(t):28s} "
            f"{str(row['unconnected_delta']):>8s} {row['snap_angle']}",
            file=stream,
        )
        if row.get("kicad") and row["kicad"].get("error"):
            print(f"    kicad error: {row['kicad']['error']}", file=stream)


def resolve_pcb(entry: dict, repo_root: str) -> str | None:
    src = entry.get("source") or {}
    rel = src.get("kicad_pcb")
    if not rel:
        return None
    path = rel if os.path.isabs(rel) else os.path.join(repo_root, rel)
    return path if os.path.exists(path) else None


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--ours", required=True, help="directory of <stem>.rust.ses")
    ap.add_argument("--fixtures", required=True, help="directory of *.dsn")
    ap.add_argument("--manifest", required=True)
    ap.add_argument("--gate", default=None, help="g21_judge gate.json (supplies u/+v)")
    ap.add_argument("--work", default=None, help="scratch dir for PCB copies + DRC JSON")
    ap.add_argument("--out", default=None)
    ap.add_argument("--kicad-cli", default=DEFAULT_KICAD_CLI)
    ap.add_argument("--kicad-py", default=DEFAULT_KICAD_PY)
    ap.add_argument("--skip-kicad", action="store_true",
                    help="angle-check only (used by tests and when KiCad is busy)")
    args = ap.parse_args(argv)

    with open(args.manifest, "r", encoding="utf-8") as f:
        manifest = json.load(f)
    gate_by = {}
    if args.gate:
        with open(args.gate, "r", encoding="utf-8") as f:
            for row in json.load(f):
                gate_by[row["fixture"]] = row

    work_root = args.work or os.path.join(
        os.path.dirname(os.path.abspath(args.ours)), "g4-work"
    )
    rows = []
    for entry in manifest["fixtures"]:
        name = entry["name"]
        stem = name[: -len(".dsn")]
        dsn = os.path.join(args.fixtures, name)
        ses = os.path.join(args.ours, stem + ".rust.ses")
        pcb = resolve_pcb(entry, REPO_ROOT)
        rows.append(
            row_for(
                name, dsn, ses, pcb, gate_by.get(name),
                work_root, args.kicad_cli, args.kicad_py, args.skip_kicad,
            )
        )
    report(rows)
    out = args.out or os.path.join(os.path.dirname(os.path.abspath(args.ours)), "g4.json")
    with open(out, "w", encoding="utf-8") as f:
        json.dump(rows, f, indent=1, ensure_ascii=False)
    print(f"\nwrote {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
