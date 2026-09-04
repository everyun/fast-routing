#!/usr/bin/env python3
"""Old (contaminated) vs new (hermetic) Java reference, one row per fixture.

Reads:
  bench/results/java-reference-contaminated-20260829/fixtures-manifest.json
  bench/results/20260828-013025/results.json + logs/*.java.stdout.txt
  bench/fixtures-manifest.json                         (new)
  bench/results/java-reference/results.json            (new round 1)

Prints a markdown table: class, stability, unrouted, violations, in-tool s,
fanout/optimizer stages. Flags boards whose Java result got better.

    python3 bench/compare_java_reference.py
"""

from __future__ import annotations

import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import run_bench

BENCH = os.path.dirname(os.path.abspath(__file__))
OLD_MAN = os.path.join(
    BENCH, "results", "java-reference-contaminated-20260829",
    "fixtures-manifest.json")
NEW_MAN = os.path.join(BENCH, "fixtures-manifest.json")
OLD_RESULTS = os.path.join(BENCH, "results", "20260828-013025", "results.json")
NEW_RESULTS = os.path.join(BENCH, "results", "java-reference", "results.json")
OLD_LOGS = os.path.join(BENCH, "results", "20260828-013025", "logs")


def load_man(path):
    with open(path, encoding="utf-8") as f:
        man = json.load(f)
    return {e["name"]: e for e in man["fixtures"]}, man


def load_java_recs(path):
    if not os.path.isfile(path):
        return {}
    import manifest as m
    _meta, recs = m.load_results(path)
    return {r["fixture"]: r for r in recs if r.get("tool") == "java"}


def stages_from_log(path):
    if not os.path.isfile(path):
        return None
    with open(path, encoding="utf-8", errors="replace") as f:
        text = f.read()
    t = text.lower()
    return {
        "fanout": "fanout stage" in t,
        "router": "auto-routing stage" in t,
        "optimizer": "optimization stage" in t,
        "tool_time_s": run_bench.parse_java_log(text).get("tool_time_s"),
        "unrouted": run_bench.parse_java_log(text).get("unrouted"),
        "violations": run_bench.parse_java_log(text).get("violations"),
    }


def cell(v):
    if v is None:
        return "-"
    if isinstance(v, bool):
        return "Y" if v else "n"
    if isinstance(v, float):
        return f"{v:.2f}"
    return str(v)


def better(old_u, old_v, new_u, new_v) -> bool:
    """Java result 'got better': fewer unrouted, or same unrouted and fewer violations."""
    if old_u is None or new_u is None:
        return False
    if new_u < old_u:
        return True
    if new_u == old_u and old_v is not None and new_v is not None and new_v < old_v:
        return True
    return False


def main() -> int:
    old_idx, old_man = load_man(OLD_MAN)
    new_idx, new_man = load_man(NEW_MAN)
    old_recs = load_java_recs(OLD_RESULTS)
    new_recs = load_java_recs(NEW_RESULTS)

    names = [e["name"] for e in new_man["fixtures"]]
    rows = []
    improved = []
    class_changes = []
    stage_changes = []

    print("| fixture | old class | new class | old stab | new stab | "
          "old u/v | new u/v | old tool s | new tool s | "
          "old fan/opt | new fan/opt | note |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|")

    for name in names:
        old, new = old_idx.get(name, {}), new_idx.get(name, {})
        old_cls = (old.get("java") or {}).get("class", "?")
        new_cls = (new.get("java") or {}).get("class", "?")
        old_st = (old.get("stability") or {}).get("class", "-")
        new_st = (new.get("stability") or {}).get("class", "-")
        old_rec = old_recs.get(name, {})
        new_rec = new_recs.get(name, {})
        stem = name[: -len(".dsn")]
        old_log = stages_from_log(
            os.path.join(OLD_LOGS, f"{stem}.java.stdout.txt"))
        new_js = new_rec.get("java_settings") or {}
        old_u = (old.get("java") or {}).get("unrouted")
        old_v = (old.get("java") or {}).get("violations")
        if old_u is None and old_rec:
            old_u, old_v = old_rec.get("unrouted"), old_rec.get("violations")
        if old_u is None and old_log:
            old_u, old_v = old_log.get("unrouted"), old_log.get("violations")
        new_u = (new.get("java") or {}).get("unrouted")
        new_v = (new.get("java") or {}).get("violations")
        if new_u is None and new_rec:
            new_u, new_v = new_rec.get("unrouted"), new_rec.get("violations")
        old_t = old_rec.get("tool_time_s")
        if old_t is None and old_log:
            old_t = old_log.get("tool_time_s")
        new_t = new_rec.get("tool_time_s")
        old_fo = (
            f"{cell((old_log or {}).get('fanout'))}/"
            f"{cell((old_log or {}).get('optimizer'))}"
        )
        new_fo = (
            f"{cell(new_js.get('fanout.enabled'))}/"
            f"{cell(new_js.get('optimizer.enabled'))}"
        )
        note = []
        if old_cls != new_cls:
            note.append(f"class {old_cls}→{new_cls}")
            class_changes.append(name)
        if better(old_u, old_v, new_u, new_v):
            note.append("JAVA BETTER")
            improved.append(name)
        if old_log and new_js:
            if (not old_log.get("fanout")) and new_js.get("fanout.enabled"):
                note.append("fanout now ran")
                stage_changes.append(name)
            if (not old_log.get("optimizer")) and new_js.get("optimizer.enabled"):
                note.append("optimizer now ran")
                if name not in stage_changes:
                    stage_changes.append(name)
        print(
            f"| {name} | {old_cls.replace('java_','')} | {new_cls.replace('java_','')} "
            f"| {old_st} | {new_st} "
            f"| {cell(old_u)}/{cell(old_v)} | {cell(new_u)}/{cell(new_v)} "
            f"| {cell(old_t)} | {cell(new_t)} "
            f"| {old_fo} | {new_fo} | {'; '.join(note) or '-'} |"
        )
        rows.append(name)

    print()
    print(f"fixtures: {len(rows)}")
    print(f"class changes: {len(class_changes)} {class_changes}")
    print(f"JAVA BETTER (retract 'we beat Java' on these): {len(improved)} {improved}")
    print(f"stage newly ran: {len(stage_changes)} {stage_changes}")
    print("old coverage", old_man.get("coverage", {}).get("by_java_class"))
    print("new coverage", new_man.get("coverage", {}).get("by_java_class"))
    print("old stability", old_man.get("coverage", {}).get("by_stability_class"))
    print("new stability", new_man.get("coverage", {}).get("by_stability_class"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
