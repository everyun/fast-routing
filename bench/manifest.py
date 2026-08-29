#!/usr/bin/env python3
"""Fixture manifest generator: bench/fixtures-manifest.json.

The manifest is the bench harness's map of the upstream fixture suite. It
answers, per board, the three questions every gate in docs/ROADMAP.md asks:

  1. *Is this the same design as another fixture?* — exact `md5` dedup group
     plus a curated variant family (the suite has 46 files, 45 distinct
     bytes, ~39 independent designs).
  2. *What does this board exercise?* — coverage properties parsed straight
     out of the DSN (signal/plane layer counts, `snap_angle`, host CAD,
     `autoroute_settings`, `class_class`, pre-existing wiring, multi-pin net
     count). These are what justify the mandatory out-of-sample tier
     (ROADMAP G2.4): 40/46 boards are 2-layer, 3 have power planes, none
     declares a `snap_angle` other than 45 degrees.
  3. *What did Java do with it?* — `java_finished / java_timeout /
     java_crash / java_degenerate`, read off an existing bench run's
     `results.json`. G2.1 applies a comparative gate to `java_finished`
     boards and an absolute gate to the rest, so this classification decides
     which gate a board gets — it is data, never a guess.
  4. *How long does Java need when nothing stops it?* — the `uncapped` block,
     folded in from `bench/results/java-uncapped/*.log` (2 h cap, otherwise
     the same jar and flags as the capped baseline). A `java_timeout` row in
     (3) says only "> 300 s"; `uncapped.uncapped_wall_s` is the
     `T_java(complete)` that ROADMAP G3.1 compares against. See
     bench/analysis/measurement-campaign.md for the campaign write-up and
     its caveats.

  5. *Does Java give the same answer twice?* — the `stability` block, folded
     in from `bench/results/java-stability/<stem>.r{1,2,3}.log` (three runs of
     the capped baseline's exact command line). ROADMAP M0.5 asks for it and
     G2.1 spends it: only a `java_finished` board whose three runs agree is
     judged comparatively, because a comparative gate against a number that
     moves between runs is not a gate. Everything else — including a board
     Java routes well but not reproducibly — gets the absolute gate.

Stdlib only. Usage (from the repo root):

    python3 bench/manifest.py                     # regenerate the manifest
    python3 bench/manifest.py --check             # CI: fail if stale
    python3 bench/manifest.py --results bench/results/<stamp>/results.json
    python3 bench/manifest.py --no-uncapped       # skip the uncapped fold-in
    python3 bench/manifest.py --no-stability      # skip the stability fold-in
"""

from __future__ import annotations

import argparse
import datetime as _dt
import glob
import hashlib
import json
import os
import re
import sys

import check_ses

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BENCH_DIR = os.path.dirname(os.path.abspath(__file__))
DEFAULT_FIXTURES_DIR = "/tmp/freerouting-upstream/fixtures"
DEFAULT_RESULTS = os.path.join(BENCH_DIR, "results", "20260828-013025", "results.json")
DEFAULT_MANIFEST = os.path.join(BENCH_DIR, "fixtures-manifest.json")
DEFAULT_UNCAPPED_DIR = os.path.join(BENCH_DIR, "results", "java-uncapped")
DEFAULT_STABILITY_DIR = os.path.join(BENCH_DIR, "results", "java-stability")

# `timeout 7200` in the uncapped runner. A run whose /usr/bin/time footer
# reports a wall at or past this is a kill, not a finish, and its numbers are
# lower bounds — never cite them as T_java(complete).
UNCAPPED_CAP_S = 7200.0

# `timeout 300` in the stability runner, i.e. the capped baseline's own cap.
STABILITY_CAP_S = 300.0
STABILITY_RUNS = 3

MANIFEST_VERSION = 3

# Freerouting's DSN reader defaults to 45-degree snapping when the board
# declares no (snap_angle ...); recorded separately from the declared value so
# the coverage numbers stay honest about what the file actually says.
DEFAULT_SNAP_ANGLE = "fortyfive_degree"

# Curated near-duplicate families. Exact-md5 dedup only catches the one
# byte-identical pair (Issue110 Pajalnaja_stancija / Паяльная станция — the
# same upstream attachment uploaded twice, once transliterated); these are the
# groups that are the *same design* in different states or revisions, which is
# what the "~39 independent designs" figure in docs/ARCHITECTURE.md counts.
# Curated from upstream issue provenance + board signature (net/component
# counts), NOT derived — adding a fixture means revisiting this table.
VARIANT_FAMILIES = {
    # same board, shipped pre-routed and unrouted
    "family:103-board": [
        "Issue103-Board-Routed.dsn",
        "Issue103-Board-Unrouted.dsn",
    ],
    # same export, one hand-corrupted to reproduce a parser bug
    "family:107-freq-teiler": [
        "Issue107-freq_teiler_200kHz_kicad.dsn",
        "Issue107-freq_teiler_200kHz_kicad_bad.dsn",
    ],
    "family:143-rpi-splitter": [
        "Issue143-rpi_splitter.dsn",
        "Issue143-rpi_splitter_mod.dsn",
    ],
    # byte-identical pair (also one md5 dedup group)
    "family:110-solder-station": [
        "Issue110-Pajalnaja_stancija.dsn",
        "Issue110-Паяльная станция.dsn",
    ],
    "family:110-fr-bugtest": [
        "Issue110-testProjectFromFreeroutingBugTest.dsn",
        "Issue110-testProjectFromFreeroutingBugTest01.dsn",
    ],
    # same Z80 design reported twice, 529 nets both, minor placement revision
    "family:z80-processor": [
        "Issue187-processor.Z80.dsn",
        "Issue190-processor.Z80.dsn",
    ],
    "family:209-split": [
        "Issue209-split05.dsn",
        "Issue209-split10.dsn",
    ],
}

JAVA_CLASSES = (
    "java_finished",
    "java_timeout",
    "java_crash",
    "java_degenerate",
    "java_unknown",
)


# --------------------------------------------------------------------------
# results.json access (shared with run_bench.py)
# --------------------------------------------------------------------------


def repo_rel(path: str) -> str:
    """Repo-relative path for anything inside the tree, absolute otherwise."""
    full = os.path.abspath(path)
    return (
        os.path.relpath(full, REPO_ROOT)
        if full.startswith(REPO_ROOT + os.sep)
        else full
    )


def load_results(path: str):
    """Load a results.json. Returns (meta, records).

    Accepts both shapes: the original bare list of records, and the current
    ``{"meta": {...}, "records": [...]}`` envelope.
    """
    with open(path, "r", encoding="utf-8") as f:
        data = json.load(f)
    if isinstance(data, dict):
        return data.get("meta", {}), data.get("records", [])
    return {}, data


def load_manifest(path: str = DEFAULT_MANIFEST):
    """Load the manifest, or None when it has not been generated yet."""
    try:
        with open(path, "r", encoding="utf-8") as f:
            return json.load(f)
    except FileNotFoundError:
        return None


def index_by_fixture(man) -> dict:
    """{fixture basename -> manifest entry}; empty dict for a missing manifest."""
    if not man:
        return {}
    return {e["name"]: e for e in man.get("fixtures", [])}


# --------------------------------------------------------------------------
# Uncapped Java runs (bench/results/java-uncapped/*.log)
# --------------------------------------------------------------------------
#
# These runs merge the jar's stdout and the /usr/bin/time -l footer into one
# file, so one text blob carries both the self-reported stage numbers and the
# externally observed wall clock. Parsed here rather than in run_bench.py
# because run_bench.py imports this module, not the other way round.

# `<n> real <n> user <n> sys` — the /usr/bin/time -l footer.
RE_UNCAPPED_TIME = re.compile(
    r"([\d.]+)\s+real\s+([\d.]+)\s+user\s+([\d.]+)\s+sys"
)
RE_UNCAPPED_RSS = re.compile(r"(\d+)\s+maximum resident set size")
# `Auto-routing stage completed: ... completed in 447.39 seconds` — the stage
# word(s) only, so keys aggregate across runs (same shape as run_bench.py).
RE_UNCAPPED_STAGE = re.compile(
    r"(\w[\w-]*(?: \w[\w-]*)?) stage completed:.*?completed in ([\d.]+) seconds",
    re.IGNORECASE,
)
RE_UNCAPPED_STAGE_START = re.compile(
    r"(\w[\w-]*(?: \w[\w-]*)?) stage started", re.IGNORECASE
)
RE_UNCAPPED_PASS = re.compile(
    r"pass #(\d+).*?completed in ([\d.]+) seconds.*?"
    r"score of ([\d.,]+) \((\d+) unrouted and (\d+) violations\)",
    re.IGNORECASE,
)
RE_UNCAPPED_FINAL = re.compile(
    r"final score: ([\d.,]+) \((\d+) unrouted and (\d+) violations\)",
    re.IGNORECASE,
)
# `Auto-routing stage started on board '<hash>' for 33 unrouted items.` and the matching
# `started with 33 unrouted nets` in the completion line. This is the *only* unrouted number
# comparable with the final one — both are Java's own count on Java's own board — so it is what
# "did the reference make progress on this board?" has to be answered with. Our referee's count
# of the same board is a different measurement and may not be mixed with it.
RE_STARTED_UNROUTED = re.compile(
    r"started (?:with|on board '[^']*' for) (\d+) unrouted (?:nets|items)",
    re.IGNORECASE,
)
# The board-specific `<name>.rules` sidecar Freerouting offers to re-read.
# Reaching this line and then going silent is the modal-dialog hang described
# in bench/analysis/measurement-campaign.md, not slow routing.
RE_UNCAPPED_RULES = re.compile(r"Opening '([^']+\.rules)'")

UNCAPPED_STATUSES = ("completed", "hit_cap", "blocked", "in_flight")


def parse_uncapped_log(text: str) -> dict:
    """Parse one uncapped run log into the fields the manifest records.

    `status` is the field that decides whether `uncapped_wall_s` may be cited:

      completed  — the router stopped on its own; wall is T_java(complete)
      hit_cap    — killed at UNCAPPED_CAP_S while routing; wall is a lower bound
      blocked    — killed at UNCAPPED_CAP_S having never started a routing
                   stage (the `.rules` confirmation dialog); the wall measures
                   the harness timeout, not the board
      in_flight  — no /usr/bin/time footer yet; the run had not exited when
                   the log was read
    """
    out = {
        "uncapped_wall_s": None,
        "uncapped_unrouted": None,
        "uncapped_violations": None,
        "started_unrouted": None,
        "hit_cap": None,
        "status": "in_flight",
        "score": None,
        "passes_run": 0,
        "routing_s": None,
        "stages": {},
        "user_s": None,
        "sys_s": None,
        "max_rss_bytes": None,
        "stages_started": [],
        "rules_sidecar": None,
    }

    m = RE_UNCAPPED_TIME.search(text)
    if m:
        out["uncapped_wall_s"] = float(m.group(1))
        out["user_s"] = float(m.group(2))
        out["sys_s"] = float(m.group(3))
    m = RE_UNCAPPED_RSS.search(text)
    if m:
        out["max_rss_bytes"] = int(m.group(1))

    stage_total = 0.0
    for m in RE_UNCAPPED_STAGE.finditer(text):
        name = m.group(1).strip().lower().replace(" ", "-")
        secs = float(m.group(2))
        out["stages"][name] = round(out["stages"].get(name, 0.0) + secs, 3)
        stage_total += secs
    if out["stages"]:
        out["routing_s"] = round(stage_total, 3)

    started = []
    for m in RE_UNCAPPED_STAGE_START.finditer(text):
        name = m.group(1).strip().lower().replace(" ", "-")
        if name not in started:
            started.append(name)
    out["stages_started"] = started

    m = RE_UNCAPPED_RULES.search(text)
    if m:
        out["rules_sidecar"] = m.group(1)

    m = RE_STARTED_UNROUTED.search(text)
    if m:
        out["started_unrouted"] = int(m.group(1))

    passes = list(RE_UNCAPPED_PASS.finditer(text))
    out["passes_run"] = len(passes)

    finals = list(RE_UNCAPPED_FINAL.finditer(text))
    if finals:
        m = finals[-1]
        out["score"] = float(m.group(1).replace(",", "."))
        out["uncapped_unrouted"] = int(m.group(2))
        out["uncapped_violations"] = int(m.group(3))
    elif passes:
        m = passes[-1]
        out["score"] = float(m.group(3).replace(",", "."))
        out["uncapped_unrouted"] = int(m.group(4))
        out["uncapped_violations"] = int(m.group(5))

    wall = out["uncapped_wall_s"]
    if wall is None:
        out["status"] = "in_flight"
        out["hit_cap"] = None
    elif wall >= UNCAPPED_CAP_S:
        out["hit_cap"] = True
        out["status"] = "hit_cap" if started else "blocked"
    else:
        out["hit_cap"] = False
        out["status"] = "completed"
    return out


def load_uncapped(uncapped_dir: str = DEFAULT_UNCAPPED_DIR) -> dict:
    """{fixture basename -> parsed uncapped record} for one results directory.

    Keys are `<stem>.dsn` so they line up with manifest entry names; the
    runner names each log after the fixture stem.
    """
    out = {}
    for path in sorted(glob.glob(os.path.join(uncapped_dir, "*.log"))):
        stem = os.path.basename(path)[: -len(".log")]
        with open(path, "r", encoding="utf-8", errors="replace") as f:
            rec = parse_uncapped_log(f.read())
        rec["log"] = repo_rel(path)
        out[stem + ".dsn"] = rec
    return out


# --------------------------------------------------------------------------
# 3-run Java stability sweep (bench/results/java-stability/<stem>.r<n>.log)
# --------------------------------------------------------------------------
#
# Same jar, same flags, same 300 s cap as the capped baseline, three times per
# fixture. What the gate needs out of it is one bit — *may this board be judged
# comparatively?* — and that bit has to be conservative in both directions, so
# the raw per-run numbers are kept alongside it.

STABILITY_CLASSES = ("stable", "unstable", "no_result", "unmeasured")


def parse_stability_run(text: str) -> dict:
    """Java's self-reported outcome of one capped run."""
    out = {
        "unrouted": None,
        "violations": None,
        "score": None,
        "started_unrouted": None,
        "passes_run": len(RE_UNCAPPED_PASS.findall(text)),
        "routing_started": bool(RE_UNCAPPED_STAGE_START.search(text)),
    }
    m = RE_STARTED_UNROUTED.search(text)
    if m:
        out["started_unrouted"] = int(m.group(1))
    finals = list(RE_UNCAPPED_FINAL.finditer(text))
    if finals:
        m = finals[-1]
        out["score"] = float(m.group(1).replace(",", "."))
        out["unrouted"] = int(m.group(2))
        out["violations"] = int(m.group(3))
    return out


def summarise_stability(runs: dict, logs: dict) -> dict:
    """Fold `{run index -> parsed run}` into the block the manifest stores.

    The class is what G2.1 reads, and only `stable` unlocks the comparative
    gate:

      stable     — at least two runs produced a final score and every one of
                   them reported the same `(unrouted, violations)`
      unstable   — at least two produced one and they disagree; the board is
                   judged absolutely, exactly like a timeout
      no_result  — the runs agree that there is nothing to report: Java was
                   killed at the cap, or the file is not a routable board, so
                   it never printed a final score. Not a defect of the sweep —
                   it is the same information `java_timeout` /
                   `java_degenerate` already carry — but it is not a
                   comparable number either
      unmeasured — fewer than two runs on disk; the sweep has not covered this
                   fixture yet
    """
    indices = sorted(runs)
    results = [runs[i] for i in indices]
    scored = [r for r in results if r["unrouted"] is not None]
    keys = {(r["unrouted"], r["violations"]) for r in scored}

    if len(results) < 2:
        cls, reason = "unmeasured", f"{len(results)} of {STABILITY_RUNS} runs on disk"
    elif not scored:
        started = any(r["routing_started"] for r in results)
        cls = "no_result"
        reason = (
            f"{len(results)} runs, none reached a final score "
            + ("(killed at the cap while routing)" if started
               else "(no routing stage ever started)")
        )
    elif len(scored) < len(results):
        cls = "unstable"
        reason = (
            f"{len(scored)} of {len(results)} runs produced a final score: the "
            f"board is only sometimes finished inside the cap"
        )
    elif len(keys) > 1:
        cls = "unstable"
        reason = "runs disagree: " + ", ".join(
            f"r{i}={r['unrouted']}u/{r['violations']}v" for i, r in zip(indices, results)
        )
    else:
        cls = "stable"
        unrouted, violations = next(iter(keys))
        reason = f"{len(results)} runs agree on {unrouted}u/{violations}v"

    agreed = next(iter(keys)) if cls == "stable" else (None, None)
    return {
        "runs": len(results),
        "scored_runs": len(scored),
        "cap_s": STABILITY_CAP_S,
        "class": cls,
        "reason": reason,
        "unrouted": agreed[0],
        "violations": agreed[1],
        "score": results[0]["score"] if cls == "stable" else None,
        "started_unrouted": next(
            (r["started_unrouted"] for r in results if r["started_unrouted"] is not None),
            None,
        ),
        "per_run": {f"r{i}": runs[i] for i in indices},
        "logs": [logs[i] for i in indices],
    }


def load_stability(stability_dir: str = DEFAULT_STABILITY_DIR) -> dict:
    """{fixture basename -> stability block} for one sweep directory.

    Log names are `<stem>.r<n>.log`; a partially finished sweep is normal and
    every fixture is summarised from whatever runs it has.
    """
    runs: dict = {}
    logs: dict = {}
    for path in sorted(glob.glob(os.path.join(stability_dir, "*.r*.log"))):
        base = os.path.basename(path)[: -len(".log")]
        stem, _, index = base.rpartition(".r")
        if not stem or not index.isdigit():
            continue
        with open(path, "r", encoding="utf-8", errors="replace") as f:
            parsed = parse_stability_run(f.read())
        runs.setdefault(stem + ".dsn", {})[int(index)] = parsed
        logs.setdefault(stem + ".dsn", {})[int(index)] = repo_rel(path)
    return {
        name: summarise_stability(by_index, logs[name])
        for name, by_index in runs.items()
    }


# --------------------------------------------------------------------------
# DSN coverage properties
# --------------------------------------------------------------------------


def md5_of(path: str) -> str:
    h = hashlib.md5()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def _type_of(layer_node):
    for t in check_ses.child_nodes(layer_node, "type"):
        return (check_ses.first_atom(t) or "").lower()
    return ""


def parse_properties(dsn_path: str) -> dict:
    """Coverage properties of one DSN, parsed with check_ses's tolerant reader.

    Never raises on a non-DSN input: `Issue006-LPC18XX_43XX_SCH.dsn` is in
    fact a binary OLE compound document (an Altium schematic renamed .dsn),
    which is exactly why Java "finishes" it with 0 nets and score 0.00.
    """
    with open(dsn_path, "r", encoding="utf-8", errors="replace") as f:
        tree = check_ses.parse_sexp(f.read())

    props = {
        "is_specctra_dsn": False,
        "layers_total": 0,
        "signal_layers": 0,
        "plane_layers": 0,
        "other_layers": 0,
        "layer_names": [],
        "snap_angle_declared": None,
        "snap_angle_effective": DEFAULT_SNAP_ANGLE,
        "host_cad": None,
        "host_version": None,
        "has_autoroute_settings": False,
        "has_class_class": False,
        "has_preexisting_wiring": False,
        "has_preexisting_copper": False,
        "preexisting_wires": 0,
        "preexisting_vias": 0,
        "nets_total": 0,
        "nets_multi_pin": 0,
        "components_placed": 0,
        "parse_note": None,
    }

    structures = list(check_ses.iter_nodes(tree, "structure"))
    if not structures and not list(check_ses.iter_nodes(tree, "pcb")):
        props["parse_note"] = (
            "no (pcb ...)/(structure ...) scope — not a Specctra DSN "
            "(binary compound document renamed .dsn)"
        )
        return props
    props["is_specctra_dsn"] = True

    for structure in structures:
        for layer in check_ses.child_nodes(structure, "layer"):
            name = check_ses.first_atom(layer)
            props["layer_names"].append(name)
            kind = _type_of(layer)
            if kind == "signal":
                props["signal_layers"] += 1
            elif kind == "power":
                props["plane_layers"] += 1
            else:
                props["other_layers"] += 1
        for snap in check_ses.child_nodes(structure, "snap_angle"):
            declared = check_ses.first_atom(snap)
            if declared:
                props["snap_angle_declared"] = declared.lower()
                props["snap_angle_effective"] = declared.lower()
    props["layers_total"] = len(props["layer_names"])

    for parser_scope in check_ses.iter_nodes(tree, "parser"):
        for key in ("host_cad", "host_version"):
            for node in check_ses.child_nodes(parser_scope, key):
                props[key] = check_ses.first_atom(node)

    props["has_autoroute_settings"] = any(
        True for _ in check_ses.iter_nodes(tree, "autoroute_settings")
    )
    props["has_class_class"] = any(
        True for _ in check_ses.iter_nodes(tree, "class_class")
    )

    for wiring in check_ses.iter_nodes(tree, "wiring"):
        props["preexisting_wires"] += sum(
            1 for _ in check_ses.child_nodes(wiring, "wire")
        )
        props["preexisting_vias"] += sum(
            1 for _ in check_ses.child_nodes(wiring, "via")
        )
    props["has_preexisting_wiring"] = props["preexisting_wires"] > 0
    # Vias without wires is a real state in this suite (Issue145 ships 42
    # pre-placed vias and no traces), so "no pre-existing wiring" must not be
    # read as "empty board".
    props["has_preexisting_copper"] = (
        props["preexisting_wires"] > 0 or props["preexisting_vias"] > 0
    )

    props["components_placed"] = sum(1 for _ in check_ses.iter_nodes(tree, "place"))

    nets = check_ses.analyze_dsn(dsn_path)
    props["nets_total"] = nets["dsn_nets_total"]
    props["nets_multi_pin"] = nets["dsn_nets_multi_pin"]
    return props


# --------------------------------------------------------------------------
# Java classification
# --------------------------------------------------------------------------


def classify_java(rec, props=None) -> dict:
    """Classify one Java bench record. `props` supplies the DSN net counts.

    Degenerate = Java "finished" without ever having a routing problem to
    solve: final score 0.00, or a board it parsed no nets out of. Issue006
    (binary file masquerading as a DSN) and Issue229 (`started with 0
    unrouted nets`, 0.03 s, empty SES) are the two known instances; both look
    like clean 0-unrouted wins in the self-reported columns, which is exactly
    why they may never enter a comparative speed or quality gate.
    """
    out = {
        "class": "java_unknown",
        "reason": "no java record in the source results.json",
        "wall_s": None,
        "tool_time_s": None,
        "score": None,
        "unrouted": None,
        "violations": None,
        "checker_nets_routed": None,
        "checker_nets_multi_pin": None,
    }
    if rec is None:
        return out

    out["wall_s"] = rec.get("wall_s")
    out["tool_time_s"] = rec.get("tool_time_s")
    out["score"] = rec.get("score")
    out["unrouted"] = rec.get("unrouted")
    out["violations"] = rec.get("violations")
    check = rec.get("check") or {}
    out["checker_nets_routed"] = check.get("ses_nets_routed")
    out["checker_nets_multi_pin"] = check.get("dsn_nets_multi_pin")

    if rec.get("timed_out"):
        out["class"] = "java_timeout"
        out["reason"] = f"killed at the {rec.get('timeout_s')}s timeout"
        return out
    if rec.get("crashed"):
        out["class"] = "java_crash"
        out["reason"] = (
            f"exit_code={rec.get('exit_code')}, no parseable completion line"
        )
        return out

    nets_multi_pin = None
    if props is not None:
        nets_multi_pin = props.get("nets_multi_pin")
    elif check:
        nets_multi_pin = check.get("dsn_nets_multi_pin")

    score = rec.get("score")
    if score is not None and score == 0.0:
        out["class"] = "java_degenerate"
        out["reason"] = "finished with final score 0.00 (nothing to route)"
        return out
    if nets_multi_pin == 0:
        out["class"] = "java_degenerate"
        out["reason"] = "0 multi-pin nets parsed from the DSN"
        return out

    out["class"] = "java_finished"
    out["reason"] = None
    return out


# --------------------------------------------------------------------------
# Manifest assembly
# --------------------------------------------------------------------------


def _family_of(name: str) -> str:
    for family, members in VARIANT_FAMILIES.items():
        if name in members:
            return family
    return f"family:{name}"


def _counter(values) -> dict:
    counts: dict = {}
    for v in values:
        key = "null" if v is None else str(v)
        counts[key] = counts.get(key, 0) + 1
    return {k: counts[k] for k in sorted(counts)}


def build_manifest(
    fixtures_dir: str,
    results_path: str | None,
    uncapped_dir: str | None = None,
    stability_dir: str | None = None,
) -> dict:
    paths = sorted(
        p
        for p in glob.glob(os.path.join(fixtures_dir, "*.dsn"))
        if os.path.isfile(p)
    )
    if not paths:
        raise SystemExit(f"error: no *.dsn fixtures in {fixtures_dir}")

    uncapped = load_uncapped(uncapped_dir) if uncapped_dir else {}
    stability = load_stability(stability_dir) if stability_dir else {}

    meta, records = ({}, [])
    results_rel = None
    if results_path:
        meta, records = load_results(results_path)
        results_rel = repo_rel(results_path)
    java_by_fixture = {
        r["fixture"]: r for r in records if r.get("tool") == "java"
    }
    # Pre-envelope results.json files (a bare list) carry no meta block; the
    # run config is still recoverable per record.
    if records and not meta:
        first = records[0]
        meta = {
            "passes": first.get("passes"),
            "timeout_s": first.get("timeout_s"),
            "jar": next(
                (
                    r["cmd"][r["cmd"].index("-jar") + 1]
                    for r in records
                    if r.get("tool") == "java" and "-jar" in (r.get("cmd") or [])
                ),
                None,
            ),
        }

    # md5 -> [names], in sorted-name order so group membership is stable.
    by_md5: dict = {}
    entries = []
    for path in paths:
        name = os.path.basename(path)
        digest = md5_of(path)
        by_md5.setdefault(digest, []).append(name)
        entries.append(
            {
                "name": name,
                "md5": digest,
                "bytes": os.path.getsize(path),
                "dedup": None,  # filled below, needs all md5s
                "variant_family": _family_of(name),
                "properties": parse_properties(path),
                "java": None,  # filled below, needs properties
                "uncapped": uncapped.get(name),
                "stability": stability.get(name),
            }
        )

    for entry in entries:
        members = by_md5[entry["md5"]]
        entry["dedup"] = {
            "group": "md5:" + entry["md5"][:12],
            "size": len(members),
            "members": members,
            # canonical representative of a byte-identical group; the other
            # members are pure duplicates and must not be counted twice in
            # any suite-level aggregate
            "primary": entry["name"] == members[0],
        }
        entry["java"] = classify_java(
            java_by_fixture.get(entry["name"]), entry["properties"]
        )

    families = sorted({e["variant_family"] for e in entries})
    props = [e["properties"] for e in entries]
    coverage = {
        "fixtures": len(entries),
        "distinct_md5": len(by_md5),
        "variant_families": len(families),
        "duplicate_groups": {
            "md5:" + d[:12]: names
            for d, names in sorted(by_md5.items())
            if len(names) > 1
        },
        "by_java_class": {
            cls: sum(1 for e in entries if e["java"]["class"] == cls)
            for cls in JAVA_CLASSES
        },
        "signal_layers": _counter(p["signal_layers"] for p in props),
        "layers_total": _counter(p["layers_total"] for p in props),
        "with_plane_layers": sum(1 for p in props if p["plane_layers"] > 0),
        "snap_angle_declared": _counter(p["snap_angle_declared"] for p in props),
        "host_cad": _counter(p["host_cad"] for p in props),
        "with_autoroute_settings": sum(1 for p in props if p["has_autoroute_settings"]),
        "with_class_class": sum(1 for p in props if p["has_class_class"]),
        "with_preexisting_wiring": sum(
            1 for p in props if p["has_preexisting_wiring"]
        ),
        "with_preexisting_copper": sum(
            1 for p in props if p["has_preexisting_copper"]
        ),
        "not_specctra_dsn": [
            e["name"] for e in entries if not e["properties"]["is_specctra_dsn"]
        ],
        "stability_measured": sum(1 for e in entries if e["stability"] is not None),
        "by_stability_class": {
            cls: sum(
                1 for e in entries if (e["stability"] or {}).get("class") == cls
            )
            for cls in STABILITY_CLASSES
        },
        # The G2.1 comparative gate's entry condition, precomputed so the number
        # in the roadmap and the number the judge uses come from one place.
        "comparative_gate_eligible": sorted(
            e["name"]
            for e in entries
            if e["java"]["class"] == "java_finished"
            and (e["stability"] or {}).get("class") == "stable"
        ),
        "java_finished_not_stable": sorted(
            e["name"]
            for e in entries
            if e["java"]["class"] == "java_finished"
            and (e["stability"] or {}).get("class") != "stable"
        ),
        "uncapped_measured": sum(1 for e in entries if e["uncapped"] is not None),
        "by_uncapped_status": {
            status: sum(
                1
                for e in entries
                if (e["uncapped"] or {}).get("status") == status
            )
            for status in UNCAPPED_STATUSES
        },
        # java_timeout boards that the uncapped campaign has NOT yet resolved
        # into a T_java(complete) — G3.1 has no comparative number for these.
        "timeout_without_uncapped_wall": [
            e["name"]
            for e in entries
            if e["java"]["class"] == "java_timeout"
            and (e["uncapped"] or {}).get("status") != "completed"
        ],
    }

    return {
        "manifest_version": MANIFEST_VERSION,
        "generated_utc": _dt.datetime.now(_dt.timezone.utc).strftime(
            "%Y-%m-%dT%H:%M:%SZ"
        ),
        "generator": "bench/manifest.py",
        "fixtures_dir": fixtures_dir,
        "java_source": {
            "results": results_rel,
            "config": {
                "label": meta.get("config_label"),
                "jar": meta.get("jar"),
                "passes": meta.get("passes"),
                "timeout_s": meta.get("timeout_s"),
            },
        },
        "java_uncapped_source": {
            "dir": repo_rel(uncapped_dir) if uncapped_dir else None,
            "cap_s": UNCAPPED_CAP_S if uncapped_dir else None,
            "analysis": "bench/analysis/measurement-campaign.md",
        },
        "java_stability_source": {
            "dir": repo_rel(stability_dir) if stability_dir else None,
            "runs": STABILITY_RUNS if stability_dir else None,
            "cap_s": STABILITY_CAP_S if stability_dir else None,
        },
        "notes": [
            "dedup.group is exact-md5; variant_family is a curated table of "
            "same-design variants (VARIANT_FAMILIES in bench/manifest.py).",
            "java.class comes from java_source.results — regenerate after the "
            "-mp 9999 re-baseline (ROADMAP M0.5) or the classes go stale.",
            "stability.class is the G2.1 gate selector: only java_finished + "
            "stable is judged comparatively, everything else (unstable, "
            "no_result, unmeasured) gets the absolute gate.",
            "stability.unrouted/violations are Java's self-reported numbers, "
            "not the referee's, and are comparable only with each other and "
            "with stability.started_unrouted — never with a referee count.",
            "snap_angle_effective falls back to the reader default "
            f"({DEFAULT_SNAP_ANGLE}) when the board declares none.",
            "uncapped.uncapped_wall_s is T_java(complete) ONLY when "
            "uncapped.status == 'completed'; 'hit_cap' is a lower bound and "
            "'blocked' measures a modal-dialog hang, not the board.",
            "the uncapped campaign ran with the fanout stage inactive while "
            "java_source.results ran it on 37/46 boards — the two Java "
            "columns are not the same pipeline (measurement-campaign.md §5).",
        ],
        "coverage": coverage,
        "fixtures": entries,
    }


def dumps(man: dict) -> str:
    return json.dumps(man, indent=2, ensure_ascii=False, sort_keys=False) + "\n"


def _strip_volatile(man: dict) -> dict:
    copy = dict(man)
    copy.pop("generated_utc", None)
    return copy


def _strip_uncapped(man: dict) -> dict:
    """Drop everything derived from the uncapped logs.

    `bench/results/**/*.log` is gitignored, so a fresh checkout can regenerate
    the manifest but cannot reproduce its `uncapped` block. `--check` compares
    on this projection when the logs are absent, otherwise CI would report a
    committed manifest as stale purely for having more data than the checkout.
    """
    copy = json.loads(json.dumps(man))
    copy.pop("java_uncapped_source", None)
    copy.get("coverage", {}).pop("uncapped_measured", None)
    copy.get("coverage", {}).pop("by_uncapped_status", None)
    copy.get("coverage", {}).pop("timeout_without_uncapped_wall", None)
    for entry in copy.get("fixtures", []):
        entry.pop("uncapped", None)
    copy["notes"] = [n for n in copy.get("notes", []) if "uncapped" not in n]
    return copy


def _strip_stability(man: dict) -> dict:
    """Drop everything derived from the stability logs, for the same reason as
    [`_strip_uncapped`]: `bench/results/java-stability/*.log` is gitignored."""
    copy = json.loads(json.dumps(man))
    copy.pop("java_stability_source", None)
    for key in (
        "stability_measured",
        "by_stability_class",
        "comparative_gate_eligible",
        "java_finished_not_stable",
    ):
        copy.get("coverage", {}).pop(key, None)
    for entry in copy.get("fixtures", []):
        entry.pop("stability", None)
    copy["notes"] = [n for n in copy.get("notes", []) if "stability" not in n]
    return copy


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    ap.add_argument("--fixtures-dir", default=DEFAULT_FIXTURES_DIR)
    ap.add_argument(
        "--results", default=DEFAULT_RESULTS,
        help="bench results.json supplying the Java classification "
             f"(default: {os.path.relpath(DEFAULT_RESULTS, REPO_ROOT)})",
    )
    ap.add_argument("--no-results", action="store_true",
                    help="skip classification (all boards java_unknown)")
    ap.add_argument(
        "--uncapped-dir", default=DEFAULT_UNCAPPED_DIR,
        help="directory of uncapped Java run logs folded into fixtures[].uncapped "
             f"(default: {os.path.relpath(DEFAULT_UNCAPPED_DIR, REPO_ROOT)})",
    )
    ap.add_argument("--no-uncapped", action="store_true",
                    help="skip the uncapped fold-in (uncapped null everywhere)")
    ap.add_argument(
        "--stability-dir", default=DEFAULT_STABILITY_DIR,
        help="directory of <stem>.r<n>.log 3-run sweep logs folded into "
             "fixtures[].stability "
             f"(default: {os.path.relpath(DEFAULT_STABILITY_DIR, REPO_ROOT)})",
    )
    ap.add_argument("--no-stability", action="store_true",
                    help="skip the stability fold-in (stability null everywhere)")
    ap.add_argument("--out", default=DEFAULT_MANIFEST)
    ap.add_argument("--check", action="store_true",
                    help="do not write; exit 1 if the on-disk manifest differs")
    ap.add_argument("--print", dest="to_stdout", action="store_true",
                    help="write the manifest to stdout instead of --out")
    args = ap.parse_args(argv)

    results = None if args.no_results else args.results
    if results and not os.path.exists(results):
        print(f"error: results file not found: {results}", file=sys.stderr)
        return 2

    uncapped_dir = None if args.no_uncapped else args.uncapped_dir
    if uncapped_dir and not os.path.isdir(uncapped_dir):
        if uncapped_dir != DEFAULT_UNCAPPED_DIR:
            print(f"error: uncapped dir not found: {uncapped_dir}", file=sys.stderr)
            return 2
        # The default location is gitignored run output; absent is normal.
        print(f"note: no uncapped results at {repo_rel(uncapped_dir)}, "
              f"skipping the uncapped fold-in", file=sys.stderr)
        uncapped_dir = None

    stability_dir = None if args.no_stability else args.stability_dir
    if stability_dir and not os.path.isdir(stability_dir):
        if stability_dir != DEFAULT_STABILITY_DIR:
            print(f"error: stability dir not found: {stability_dir}", file=sys.stderr)
            return 2
        print(f"note: no stability results at {repo_rel(stability_dir)}, "
              f"skipping the stability fold-in", file=sys.stderr)
        stability_dir = None

    man = build_manifest(args.fixtures_dir, results, uncapped_dir, stability_dir)
    text = dumps(man)

    if args.to_stdout:
        sys.stdout.write(text)
        return 0

    if args.check:
        old = load_manifest(args.out)
        if old is None:
            print(f"stale: {args.out} does not exist", file=sys.stderr)
            return 1
        def project(m):
            m = _strip_volatile(m)
            if not uncapped_dir:
                m = _strip_uncapped(m)
            if not stability_dir:
                m = _strip_stability(m)
            return m

        if project(old) != project(man):
            print(f"stale: {args.out} differs from a fresh generation "
                  f"(run: python3 bench/manifest.py)", file=sys.stderr)
            return 1
        skipped = [
            name
            for name, present in (("uncapped", uncapped_dir), ("stability", stability_dir))
            if not present
        ]
        scope = f" ({' and '.join(skipped)} fields not compared)" if skipped else ""
        print(f"ok: {args.out} is up to date{scope}")
        return 0

    with open(args.out, "w", encoding="utf-8") as f:
        f.write(text)
    cov = man["coverage"]
    print(f"wrote {args.out}")
    print(f"  {cov['fixtures']} fixtures, {cov['distinct_md5']} distinct md5, "
          f"{cov['variant_families']} variant families")
    print("  java classes: " + ", ".join(
        f"{k}={v}" for k, v in cov["by_java_class"].items() if v
    ))
    print(f"  uncapped: {cov['uncapped_measured']} measured (" + ", ".join(
        f"{k}={v}" for k, v in cov["by_uncapped_status"].items() if v
    ) + ")")
    still = cov["timeout_without_uncapped_wall"]
    if still:
        print(f"  no T_java(complete) yet for {len(still)}: " + ", ".join(still))
    print(f"  stability: {cov['stability_measured']} measured (" + ", ".join(
        f"{k}={v}" for k, v in cov["by_stability_class"].items() if v
    ) + ")")
    print(f"  G2.1 comparative-gate eligible: "
          f"{len(cov['comparative_gate_eligible'])} of {cov['fixtures']}")
    demoted = cov["java_finished_not_stable"]
    if demoted:
        print(f"  java_finished but not stable ({len(demoted)}): " + ", ".join(demoted))
    return 0


if __name__ == "__main__":
    sys.exit(main())
