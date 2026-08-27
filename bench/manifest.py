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

The `stability` field is deliberately left `null`: it gets filled by the
3-run Java sweep in ROADMAP M0.5 ("record 3-run stability per fixture").

Stdlib only. Usage (from the repo root):

    python3 bench/manifest.py                     # regenerate the manifest
    python3 bench/manifest.py --check             # CI: fail if stale
    python3 bench/manifest.py --results bench/results/<stamp>/results.json
"""

from __future__ import annotations

import argparse
import datetime as _dt
import glob
import hashlib
import json
import os
import sys

import check_ses

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BENCH_DIR = os.path.dirname(os.path.abspath(__file__))
DEFAULT_FIXTURES_DIR = "/tmp/freerouting-upstream/fixtures"
DEFAULT_RESULTS = os.path.join(BENCH_DIR, "results", "20260828-013025", "results.json")
DEFAULT_MANIFEST = os.path.join(BENCH_DIR, "fixtures-manifest.json")

MANIFEST_VERSION = 1

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


def build_manifest(fixtures_dir: str, results_path: str | None) -> dict:
    paths = sorted(
        p
        for p in glob.glob(os.path.join(fixtures_dir, "*.dsn"))
        if os.path.isfile(p)
    )
    if not paths:
        raise SystemExit(f"error: no *.dsn fixtures in {fixtures_dir}")

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
                "stability": None,
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
        "notes": [
            "dedup.group is exact-md5; variant_family is a curated table of "
            "same-design variants (VARIANT_FAMILIES in bench/manifest.py).",
            "java.class comes from java_source.results — regenerate after the "
            "-mp 9999 re-baseline (ROADMAP M0.5) or the classes go stale.",
            "stability is null everywhere until the 3-run Java sweep lands; "
            "ROADMAP G2.1 treats unstable boards like timeout/crash boards.",
            "snap_angle_effective falls back to the reader default "
            f"({DEFAULT_SNAP_ANGLE}) when the board declares none.",
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

    man = build_manifest(args.fixtures_dir, results)
    text = dumps(man)

    if args.to_stdout:
        sys.stdout.write(text)
        return 0

    if args.check:
        old = load_manifest(args.out)
        if old is None:
            print(f"stale: {args.out} does not exist", file=sys.stderr)
            return 1
        if _strip_volatile(old) != _strip_volatile(man):
            print(f"stale: {args.out} differs from a fresh generation "
                  f"(run: python3 bench/manifest.py)", file=sys.stderr)
            return 1
        print(f"ok: {args.out} is up to date")
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
    return 0


if __name__ == "__main__":
    sys.exit(main())
