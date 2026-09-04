#!/usr/bin/env python3
"""ROADMAP G4: KiCad product gate.

For every out-of-sample (or any) board:

  * count SES wire-segment angle violations against the board's snap_angle
    (`none` / `ninety_degree` / `fortyfive_degree`, defaulting to 45° when
    the DSN declares nothing — same default as the Specctra reader),
    split into `generated` (router-produced — the G4 number) and `imported`
    (verbatim re-emitted pre-routed DSN copper, which no Specctra router
    re-angles);

and for every board that has a `.kicad_pcb` source:

  * copy the PCB, run `kicad-cli pcb drc` before SES import,
    `pcbnew.ImportSpecctraSES` the session into the copy,
    refill every zone with `pcbnew.ZONE_FILLER` (KiCad does not refill on
    SES import, so without this step DRC checks our fresh tracks against
    the fixture's *stale* zone fills — see REPORT-easyeda class (c)),
    then run DRC three times and intersect the violations by physical
    object pair (item UUIDs), not coordinates: kicad-cli DRC is not
    run-to-run stable on some boards.

The DRC-violation delta is reported in four columns:

  * `raw`          — after − before violation count, first DRC run;
  * `fixture_debt` — stable after-violations whose every item UUID already
                     exists in the fixture PCB (pre-existing/locked copper,
                     not ours);
  * `noise`        — violations of the first run that do not reproduce in
                     all three runs;
  * `residual`     — stable violations involving at least one object the
                     fixture did not contain. This is the gate number.

The G4 4-tuple is (unrouted, added violations, angle violations, KiCad-DRC
residual). The first two numbers come from a G2.1 `gate.json` when one is
passed; they are `null` otherwise. This file never talks to the router.

Stdlib Python 3 only, like the rest of `bench/`. pcbnew / kicad-cli are
optional at import time so the unit tests stay hermetic.
"""

from __future__ import annotations

import argparse
import collections
import json
import math
import os
import re
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
# Per-coordinate rounding of a half-integer 45° corner (upstream's
# `smoothen_non_integer_corner` produces them legitimately) shifts each axis by
# ≤ 0.5 session units per endpoint, so a faithful diagonal can land with
# ||dx| − |dy|| ≤ 2. The 2.3.0 jar's own sessions contain such segments — e.g.
# Issue187-processor.Z80.java.ses has 20, at the same coordinates as ours — so
# the gate absorbs that slack (capped at ~1.1° of arc, so short segments stay
# strict) instead of demanding the router beat the jar.
DIAG_ROUNDING_SLACK = 2.0  # session units, applied to the |dx| == |dy| test


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
    """(path <layer> <width> x1 y1 x2 y2 ...) -> [x1, y1, x2, y2, ...].

    Layer and width are dropped **positionally**: the layer atom may be a
    quoted *numeric* name (EasyEDA exports `"1"`, `"2"`, …), which the parser
    returns unquoted and indistinguishable from a coordinate. Filtering by
    float-parsability would eat it and shift every coordinate by one.
    """
    atoms = [el for el in path_node[1:] if isinstance(el, str)]
    return [float(a) for a in atoms[2:]]


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
    # fortyfive_degree (reader default) and anything we don't recognise. The
    # slack absorbs coordinate rounding (≤ 2 session units) but only where it
    # is actually rounding-scale: within ~1.1° of the exact diagonal.
    slack = min(DIAG_ROUNDING_SLACK, 0.02 * length)
    diag = abs(abs(dx) - abs(dy)) <= max(eps * length, slack)
    return axis or diag


def _dsn_imported_segments(dsn_path: str) -> set[tuple[float, ...]]:
    """Every `(wire (path …))` segment in the DSN's wiring scope, scaled to SES
    coordinates, both directions.

    The reader scales DSN coordinates by the board scale factor, which is the
    resolution value times a power of ten (`dsn-board-semantics.md` §3.2), so
    all candidates `r * 10^k` are included rather than re-deriving k.
    """
    with open(dsn_path, "r", encoding="utf-8", errors="replace") as f:
        tree = check_ses.parse_sexp(f.read())
    resolution = 1.0
    for node in check_ses.iter_nodes(tree, "resolution"):
        if len(node) >= 3:
            try:
                resolution = float(node[2])
            except (TypeError, ValueError):
                pass
            break
    scales = [resolution * (10.0**k) for k in range(4)]
    segs: set[tuple[float, ...]] = set()
    for wire in check_ses.iter_nodes(tree, "wire"):
        for path in check_ses.iter_nodes(wire, "path"):
            pts = _nums_after_width(path)
            for k in range(2, len(pts) - 1, 2):
                for s in scales:
                    seg = (pts[k - 2] * s, pts[k - 1] * s, pts[k] * s, pts[k + 1] * s)
                    segs.add(seg)
                    segs.add((seg[2], seg[3], seg[0], seg[1]))
    return segs


def angle_violations(ses_path: str, snap: str, dsn_path: str | None = None) -> dict:
    """Walk every SES wire path; count illegal segments.

    A zero-length hop is ignored. When the source DSN is given, each illegal
    segment is additionally classified as `imported` (a verbatim re-emission
    of pre-routed DSN copper, which upstream also writes back unchanged) or
    `generated` (produced by the router — the number G4 gates on). Returns the
    counts plus a few examples so a failing board is diagnosable without
    re-parsing the file.
    """
    imported_segs: set[tuple[float, ...]] = set()
    have_dsn = bool(dsn_path) and os.path.exists(dsn_path)
    if have_dsn:
        imported_segs = _dsn_imported_segments(dsn_path)
    with open(ses_path, "r", encoding="utf-8", errors="replace") as f:
        tree = check_ses.parse_sexp(f.read())
    bad = 0
    imported = 0
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
                    seg = (pts[k - 2], pts[k - 1], pts[k], pts[k + 1])
                    if seg in imported_segs:
                        imported += 1
                    if len(examples) < 8:
                        examples.append(
                            {
                                "from": [pts[k - 2], pts[k - 1]],
                                "to": [pts[k], pts[k + 1]],
                                "dx": dx,
                                "dy": dy,
                                "imported": seg in imported_segs,
                            }
                        )
    return {
        "snap_angle": snap,
        "segments": total,
        "angle_violations": bad,
        "angle_violations_imported": imported if have_dsn else None,
        "angle_violations_generated": (bad - imported) if have_dsn else None,
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
# violation classification (fixture debt / run noise / residual)
# --------------------------------------------------------------------------

_UUID_RE = re.compile(r"\((?:uuid|tstamp)\s+\"?([0-9A-Fa-f-]{36})\"?\)")


def pcb_uuids(pcb_path: str) -> set:
    """Every item UUID in a .kicad_pcb (uuid nodes; tstamp for older files)."""
    with open(pcb_path, "r", encoding="utf-8", errors="replace") as f:
        return set(_UUID_RE.findall(f.read()))


def _pcb_copper(pcb_path: str):
    """(segments, vias) geometry of a .kicad_pcb, in mm.

    segments: [((x1, y1), (x2, y2))]; vias: [(x, y)].
    """
    with open(pcb_path, "r", encoding="utf-8", errors="replace") as f:
        tree = check_ses.parse_sexp(f.read())
    segs = []
    for s in check_ses.iter_nodes(tree, "segment"):
        st = next((c for c in s if isinstance(c, list) and c and c[0] == "start"), None)
        en = next((c for c in s if isinstance(c, list) and c and c[0] == "end"), None)
        if st and en:
            segs.append(((float(st[1]), float(st[2])), (float(en[1]), float(en[2]))))
    vias = []
    for v in check_ses.iter_nodes(tree, "via"):
        at = next((c for c in v if isinstance(c, list) and c and c[0] == "at"), None)
        if at:
            vias.append((float(at[1]), float(at[2])))
    return segs, vias


def _pcb_geometry_by_uuid(pcb_path: str) -> dict:
    """uuid -> ("seg", x1, y1, x2, y2) | ("via", x, y) for tracks and vias."""
    with open(pcb_path, "r", encoding="utf-8", errors="replace") as f:
        tree = check_ses.parse_sexp(f.read())

    def uuid_of(node):
        for c in node:
            if isinstance(c, list) and c and c[0] in ("uuid", "tstamp"):
                return str(c[1])
        return None

    out = {}
    for s in check_ses.iter_nodes(tree, "segment"):
        u = uuid_of(s)
        st = next((c for c in s if isinstance(c, list) and c and c[0] == "start"), None)
        en = next((c for c in s if isinstance(c, list) and c and c[0] == "end"), None)
        if u and st and en:
            out[u] = ("seg", float(st[1]), float(st[2]), float(en[1]), float(en[2]))
    for v in check_ses.iter_nodes(tree, "via"):
        u = uuid_of(v)
        at = next((c for c in v if isinstance(c, list) and c and c[0] == "at"), None)
        if u and at:
            out[u] = ("via", float(at[1]), float(at[2]))
    return out


class FixtureCopper:
    """Decides whether a DRC violation item is the fixture's own copper.

    An item is fixture-owned when its UUID survives from the fixture PCB —
    footprints, pads, zones, Edge.Cuts, locked tracks — **or**, for tracks and
    vias, when the same geometry existed before the import: KiCad's SES import
    deletes unlocked wiring and re-creates it from the session with *fresh*
    UUIDs, so the fixture's own preroute debt round-trips under new identities
    (REPORT-easyeda caveat). Pads never need the geometric fallback — SES
    import does not touch footprints.
    """

    SEG_TOL_MM = 0.01
    VIA_TOL_MM = 0.01

    def __init__(self, before_pcb: str, after_pcb: str):
        self.before_uuids = pcb_uuids(before_pcb)
        self.before_segs, self.before_vias = _pcb_copper(before_pcb)
        self.after_by_uuid = _pcb_geometry_by_uuid(after_pcb)

    def owns(self, item: dict) -> bool:
        uuid = item.get("uuid")
        if not uuid:
            return False
        if uuid in self.before_uuids:
            return True
        geo = self.after_by_uuid.get(uuid)
        if geo is None:
            return False
        if geo[0] == "via":
            _, x, y = geo
            tol = self.VIA_TOL_MM
            return any(math.hypot(x - vx, y - vy) <= tol for vx, vy in self.before_vias)
        _, x1, y1, x2, y2 = geo
        tol = self.SEG_TOL_MM
        for (ax, ay), (bx, by) in self.before_segs:
            forward = math.hypot(x1 - ax, y1 - ay) <= tol and math.hypot(x2 - bx, y2 - by) <= tol
            reverse = math.hypot(x1 - bx, y1 - by) <= tol and math.hypot(x2 - ax, y2 - ay) <= tol
            if forward or reverse:
                return True
        return False


def _item_key(item: dict) -> str:
    """Identity of one DRC item: its UUID when present, else rounded position."""
    uuid = item.get("uuid")
    if uuid:
        return f"uuid:{uuid}"
    pos = item.get("pos") or {}
    return f"pos:{pos.get('x', 0):.4f},{pos.get('y', 0):.4f}"


def violation_key(violation: dict):
    """Physical object pair: violation type + sorted item identities.

    Coordinates are deliberately not part of the key — kicad-cli DRC jitter
    moves reported positions between runs without changing the objects.
    """
    return (
        (violation.get("type") or violation.get("error_type") or "").lower(),
        tuple(sorted(_item_key(it) for it in violation.get("items") or [])),
    )


def _is_fixture_debt(violation: dict, fixture: FixtureCopper) -> bool:
    """True iff every item of the violation is the fixture's own copper."""
    items = violation.get("items") or []
    if not items:
        return False
    return all(fixture.owns(it) for it in items)


def classify_violations(after_runs: list, before_report: dict, fixture) -> dict:
    """Split the after-DRC violations into noise / fixture-debt / residual.

    `after_runs` are ≥1 DRC reports of the same imported board. A violation
    is *stable* when its physical object pair shows up in every run; the
    rest of the first run is run-to-run noise. Among the stable ones,
    violations whose items all pre-date the SES import are fixture debt
    (`fixture` is a [`FixtureCopper`], or any object with an `owns(item)`
    predicate); the remainder is the residual attributable to our routing.
    """
    before = count_drc(before_report)
    run_maps = []
    for rep in after_runs:
        counted = count_drc(rep)
        m = {}
        # re-walk the raw list so aggregated unconnected entries stay out
        for v in rep.get("violations") or []:
            vtype = (v.get("type") or v.get("error_type") or "").lower()
            if vtype in ("unconnected_items", "unconnected_item", "unconnected"):
                continue
            m[violation_key(v)] = v
        m["_count"] = counted["violations"]
        run_maps.append(m)
    first = run_maps[0]
    first_keys = {k for k in first if k != "_count"}
    stable = set(first_keys)
    for m in run_maps[1:]:
        stable &= {k for k in m if k != "_count"}
    noise_keys = first_keys - stable
    debt, residual = [], []
    for k in sorted(stable):
        (debt if _is_fixture_debt(first[k], fixture) else residual).append(k)
    return {
        "raw_delta": first["_count"] - before["violations"],
        "noise": len(noise_keys),
        "fixture_debt": len(debt),
        "residual": len(residual),
        "residual_violations": [
            {
                "type": k[0],
                "description": (first[k].get("description") or "")[:120],
                "items": [
                    (it.get("description") or "")[:80] for it in first[k].get("items") or []
                ],
            }
            for k in residual[:50]
        ],
        "noise_examples": [
            {"type": k[0], "description": (first[k].get("description") or "")[:120]}
            for k in sorted(noise_keys)[:10]
        ],
    }


# --------------------------------------------------------------------------
# zone refill (filled_polygon before/after comparison)
# --------------------------------------------------------------------------


def zone_fill_report(prerefill_pcb: str, refilled_pcb: str) -> dict:
    """Did ZONE_FILLER change any filled_polygon? Compared as multisets."""
    polys = []
    for path in (prerefill_pcb, refilled_pcb):
        with open(path, "r", encoding="utf-8", errors="replace") as f:
            tree = check_ses.parse_sexp(f.read())
        polys.append(
            collections.Counter(repr(n) for n in check_ses.iter_nodes(tree, "filled_polygon"))
        )
    diff = polys[1] - polys[0]
    diff.update(polys[0] - polys[1])
    return {
        "filled_polygons_before": sum(polys[0].values()),
        "filled_polygons_after": sum(polys[1].values()),
        "changed": bool(diff),
        "changed_polygons": sum(diff.values()),
    }


# --------------------------------------------------------------------------
# pcbnew SES import + kicad-cli DRC (skipped in unit tests)
# --------------------------------------------------------------------------


KICAD_TIMEOUT_S = 300  # one wedged kicad-cli/pcbnew child must not wedge the suite


def run_drc(kicad_cli: str, pcb_path: str, out_json: str) -> dict:
    cmd = [
        kicad_cli, "pcb", "drc",
        "--format", "json",
        "--severity-error",
        "-o", out_json,
        pcb_path,
    ]
    done = subprocess.run(
        cmd, capture_output=True, text=True, errors="replace", timeout=KICAD_TIMEOUT_S
    )
    if not os.path.exists(out_json):
        raise RuntimeError(
            f"kicad-cli pcb drc produced no report (exit {done.returncode}): "
            f"{done.stderr[-400:]}"
        )
    with open(out_json, "r", encoding="utf-8") as f:
        return json.load(f)


def import_ses(
    kicad_py: str,
    pcb_path: str,
    ses_path: str,
    out_pcb: str,
    prerefill_pcb: str | None = None,
) -> None:
    """Import the SES, then refill all zones before saving.

    KiCad's SES importer adds tracks/vias but never refills zones; without
    the ZONE_FILLER pass the after-DRC checks new copper against stale
    fixture fills. `prerefill_pcb` keeps a pre-refill copy so the caller can
    prove the refill actually changed filled_polygons.
    """
    script = (
        "import wx\n"
        "app = wx.App(False)\n"
        "import pcbnew\n"
        f"board = pcbnew.LoadBoard({pcb_path!r})\n"
        f"ok = pcbnew.ImportSpecctraSES(board, {ses_path!r})\n"
        "if not ok:\n"
        "    raise SystemExit('ImportSpecctraSES returned False')\n"
        + (f"pcbnew.SaveBoard({prerefill_pcb!r}, board)\n" if prerefill_pcb else "")
        + "pcbnew.ZONE_FILLER(board).Fill(board.Zones())\n"
        f"pcbnew.SaveBoard({out_pcb!r}, board)\n"
        # Skip wx teardown: wxLogGui flushes buffered import warnings as a
        # modal NSAlert during wxEntryCleanup, which wedges a headless run
        # (observed on kicad-tutorial; sample stack ends in -[NSAlert runModal]).
        "import os\n"
        "os._exit(0)\n"
    )
    done = subprocess.run(
        [kicad_py, "-c", script],
        capture_output=True, text=True, errors="replace", timeout=KICAD_TIMEOUT_S,
    )
    if done.returncode != 0:
        raise RuntimeError(
            f"ImportSpecctraSES failed (exit {done.returncode}): "
            f"{(done.stderr or done.stdout)[-600:]}"
        )


DRC_RUNS = 3  # after-DRC repetitions for the noise intersection


def kicad_roundtrip(
    pcb_path: str,
    ses_path: str,
    work_dir: str,
    kicad_cli: str,
    kicad_py: str,
    drc_runs: int = DRC_RUNS,
    reuse_drc: bool = False,
) -> dict:
    """DRC before, import SES, refill zones, DRC after ×`drc_runs`.

    All artefacts stay in `work_dir`. Returns the raw delta plus the
    noise / fixture-debt / residual split of the after-violations.
    With `reuse_drc`, each step whose artefacts already exist in `work_dir`
    is skipped and the stored files are loaded instead — DRC runs and the
    wx import are expensive; classification is not. A step with incomplete
    artefacts is re-run (so a board whose import wedged after saving can
    reuse the saved PCBs and only re-run its DRCs).
    """
    os.makedirs(work_dir, exist_ok=True)
    before_pcb = os.path.join(work_dir, "before.kicad_pcb")
    after_pcb = os.path.join(work_dir, "after.kicad_pcb")
    prerefill_pcb = os.path.join(work_dir, "after-norefill.kicad_pcb")
    before_json = os.path.join(work_dir, "drc-before.json")
    after_jsons = [
        os.path.join(work_dir, f"drc-after-r{i + 1}.json") for i in range(max(1, drc_runs))
    ]

    def load_json(path):
        with open(path, "r", encoding="utf-8") as f:
            return json.load(f)

    if reuse_drc and os.path.exists(before_pcb) and os.path.exists(before_json):
        before = load_json(before_json)
    else:
        shutil.copy2(pcb_path, before_pcb)
        before = run_drc(kicad_cli, before_pcb, before_json)
    imported = os.path.exists(after_pcb) and os.path.exists(prerefill_pcb)
    if not (reuse_drc and imported):
        import_ses(kicad_py, before_pcb, ses_path, after_pcb, prerefill_pcb)
    after_runs = []
    for path in after_jsons:
        if reuse_drc and imported and os.path.exists(path):
            after_runs.append(load_json(path))
        else:
            after_runs.append(run_drc(kicad_cli, after_pcb, path))
    delta = drc_delta(before, after_runs[0])
    cls = classify_violations(after_runs, before, FixtureCopper(before_pcb, after_pcb))
    delta["zone_refill"] = zone_fill_report(prerefill_pcb, after_pcb)
    delta["drc_runs"] = max(1, drc_runs)
    delta["drc_delta_raw"] = cls["raw_delta"]
    delta["drc_noise"] = cls["noise"]
    delta["drc_fixture_debt"] = cls["fixture_debt"]
    delta["drc_residual"] = cls["residual"]
    delta["residual_violations"] = cls["residual_violations"]
    delta["noise_examples"] = cls["noise_examples"]
    delta["drc_before_path"] = before_json
    delta["drc_after_paths"] = after_jsons
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
    reuse_drc: bool = False,
) -> dict:
    snap = snap_angle_of(dsn) if os.path.exists(dsn) else DEFAULT_SNAP
    have_ses = ses and os.path.exists(ses) and os.path.getsize(ses) > 0
    angles = (
        angle_violations(ses, snap, dsn)
        if have_ses
        else {"snap_angle": snap, "segments": 0, "angle_violations": None,
              "angle_violations_imported": None, "angle_violations_generated": None,
              "examples": []}
    )
    kicad = None
    if pcb and not skip_kicad and ses and os.path.exists(ses) and os.path.getsize(ses) > 0:
        work = os.path.join(work_root or tempfile.mkdtemp(prefix="g4-"), name)
        try:
            kicad = kicad_roundtrip(pcb, ses, work, kicad_cli, kicad_py, reuse_drc=reuse_drc)
        except Exception as e:  # noqa: BLE001 — one board must not kill the suite
            kicad = {"error": f"{type(e).__name__}: {e}"}
    elif pcb and skip_kicad:
        kicad = {"skipped": True}
    elif not pcb:
        kicad = {"skipped": True, "reason": "no kicad_pcb source"}

    unrouted = (gate_row or {}).get("ours_unrouted")
    added = (gate_row or {}).get("added_violations")
    # The tuple's angle number is the count the router is responsible for:
    # generated segments when the DSN is available to classify with, else the
    # raw total.
    angle_n = angles.get("angle_violations_generated")
    if angle_n is None:
        angle_n = angles.get("angle_violations")
    kicad_ok = kicad and "error" not in kicad and not kicad.get("skipped")
    drc_n = kicad["drc_residual"] if kicad_ok else None
    return {
        "fixture": name,
        "tuple": four_tuple(unrouted, added, angle_n, drc_n),
        "unrouted": unrouted,
        "added_violations": added,
        "angle_violations": angle_n,
        "angle_violations_imported": angles.get("angle_violations_imported"),
        "kicad_drc_residual": drc_n,
        "kicad_drc_raw": kicad["drc_delta_raw"] if kicad_ok else None,
        "kicad_drc_fixture_debt": kicad["drc_fixture_debt"] if kicad_ok else None,
        "kicad_drc_noise": kicad["drc_noise"] if kicad_ok else None,
        "unconnected_delta": kicad["unconnected_delta"] if kicad_ok else None,
        "snap_angle": snap,
        "angles": angles,
        "kicad": kicad,
        "ses": ses,
        "pcb": pcb,
        "gate": (gate_row or {}).get("verdict"),
    }


def report(rows, stream=sys.stdout):
    print(
        f"{'fixture':36s} {'tuple (u, +v, ang, drc)':28s} "
        f"{'rawΔ':>6s} {'debt':>5s} {'noise':>6s} {'resid':>6s} {'unconnΔ':>8s} snap",
        file=stream,
    )
    for row in rows:
        t = row["tuple"]
        imported = row.get("angle_violations_imported")
        tag = f" (+{imported} imported)" if imported else ""
        print(
            f"{row['fixture'][:34]:36s} {str(t):28s} "
            f"{str(row['kicad_drc_raw']):>6s} {str(row['kicad_drc_fixture_debt']):>5s} "
            f"{str(row['kicad_drc_noise']):>6s} {str(row['kicad_drc_residual']):>6s} "
            f"{str(row['unconnected_delta']):>8s} {row['snap_angle']}{tag}",
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
    ap.add_argument("--reuse-drc", action="store_true",
                    help="re-classify stored --work artefacts without re-running KiCad")
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
                reuse_drc=args.reuse_drc,
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
