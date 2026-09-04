#!/usr/bin/env python3
"""Re-derive every number in bench/analysis/measurement-campaign.md from logs.

The write-up is prose around tables; this script is the tables. Running it
prints exactly the markdown that belongs in the .md, so a stale write-up is a
diff away from being caught:

    python3 bench/analysis/campaign.py            # all sections
    python3 bench/analysis/campaign.py --section uncapped

Inputs (all read-only):

  bench/results/java-uncapped-contaminated-20260828/*.log
                                           the 2026-08-28 uncapped 2 h runs of
                                           the 14 boards the 300 s baseline
                                           could not finish (fanout/optimizer
                                           off — the pre-isolation pipeline)
  bench/results/java-uncapped/*.log        the 2026-09-05 hermetic re-run
                                           (fresh --user_data_path per board)
  bench/results/java-experiments/*.log     SerialGC / DEBUG-console probes
  bench/results/20260828-013025/           the 300 s capped baseline

Stdlib only. Log parsing for the uncapped runs lives in bench/manifest.py
(`parse_uncapped_log`) so the manifest and this script can never disagree.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys

BENCH_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REPO_ROOT = os.path.dirname(BENCH_DIR)
sys.path.insert(0, BENCH_DIR)

import manifest as fixture_manifest  # noqa: E402

UNCAPPED_DIR = os.path.join(
    BENCH_DIR, "results", "java-uncapped-contaminated-20260828")
HERMETIC_UNCAPPED_DIR = os.path.join(BENCH_DIR, "results", "java-uncapped")
EXPERIMENTS_DIR = os.path.join(BENCH_DIR, "results", "java-experiments")
BASELINE_DIR = os.path.join(BENCH_DIR, "results", "20260828-013025")
STABILITY_DIR = os.path.join(BENCH_DIR, "results", "java-stability")

# The five boards phase 2 of /tmp/frbase/measure.sh actually reached. slow6.txt
# lists six; its last line has no trailing newline, so `while read -r` dropped
# Issue054-tairakb.dsn silently — there is no SerialGC run for it.
SERIALGC_FIXTURES = (
    "Issue209-split05",
    "Issue190-processor.Z80",
    "Issue039-bug-design",
    "Issue269-caniot-tiny-arm",
    "Issue157-TeamAdapt-LinePCB",
)

FANOUT_PASS = re.compile(
    r"Fanout pass #(\d+) on board '(\w+)' completed in ([\d.]+) seconds with "
    r"(\d+) SMD pins fanouted, (\d+) not routed, (\d+) insert errors, "
    r"\+(\d+) extra vias"
)
FANOUT_STAGE = re.compile(
    r"Fanout stage completed: started with (\d+) total SMD pins, completed in "
    r"([\d.]+) seconds, escaped pins: (\d+)/(\d+)"
)
ALLOC_GB = re.compile(r"([\d.]+) GB total allocated")
FIRST_TS = re.compile(r"^(\d{4}-\d\d-\d\d \d\d:\d\d:\d\d)\.\d+", re.MULTILINE)


def read(path: str) -> str:
    with open(path, "r", encoding="utf-8", errors="replace") as f:
        return f.read()


def baseline_java() -> dict:
    _meta, records = fixture_manifest.load_results(
        os.path.join(BASELINE_DIR, "results.json")
    )
    return {r["fixture"]: r for r in records if r.get("tool") == "java"}


def baseline_rust() -> dict:
    _meta, records = fixture_manifest.load_results(
        os.path.join(BASELINE_DIR, "results.json")
    )
    return {r["fixture"]: r for r in records if r.get("tool") == "rust"}


def table(header, rows) -> str:
    out = ["| " + " | ".join(header) + " |",
           "|" + "|".join("---" for _ in header) + "|"]
    out += ["| " + " | ".join(str(c) for c in row) + " |" for row in rows]
    return "\n".join(out)


def _fmt(x, spec="%.2f", dash="-"):
    return dash if x is None else spec % x


# --------------------------------------------------------------------------
# Section 1: uncapped runs
# --------------------------------------------------------------------------


def section_uncapped() -> str:
    unc = fixture_manifest.load_uncapped(UNCAPPED_DIR)
    rust = baseline_rust()
    rows = []
    for name in sorted(unc):
        u = unc[name]
        stages = u["stages"]
        stage_cells = ", ".join(f"{k} {v:.2f}" for k, v in sorted(stages.items()))
        r = rust.get(name) or {}
        rust_cell = (
            "TIMEOUT" if r.get("timed_out")
            else _fmt(r.get("wall_s"), "%.2f s")
        )
        rows.append([
            name.replace(".dsn", ""),
            _fmt(u["uncapped_wall_s"]),
            stage_cells or "-",
            u["passes_run"] or "-",
            _fmt(u["score"]),
            u["uncapped_unrouted"] if u["uncapped_unrouted"] is not None else "-",
            u["uncapped_violations"] if u["uncapped_violations"] is not None else "-",
            "**yes**" if u["hit_cap"] else "no",
            u["status"],
            rust_cell,
        ])
    completed = [u for u in unc.values() if u["status"] == "completed"]
    total = sum(u["uncapped_wall_s"] for u in unc.values() if u["uncapped_wall_s"])
    summary = (
        f"\n{len(completed)}/{len(unc)} completed; "
        f"completed wall sum {sum(u['uncapped_wall_s'] for u in completed):.2f} s, "
        f"max {max(u['uncapped_wall_s'] for u in completed):.2f} s; "
        f"campaign wall (incl. capped) {total:.2f} s = {total / 3600:.2f} h."
    )
    return table(
        ["fixture", "wall s", "stage times s", "passes", "score",
         "unrouted", "violations", "hit 2 h cap", "status", "rust wall (300 s run)"],
        rows,
    ) + summary


def section_gc_surplus() -> str:
    """user-CPU above wall on a router that reports 'on 1 thread'."""
    unc = fixture_manifest.load_uncapped(UNCAPPED_DIR)
    rows = []
    for name in sorted(unc):
        u = unc[name]
        if u["status"] != "completed":
            continue
        rows.append([
            name.replace(".dsn", ""),
            "%.2f" % u["uncapped_wall_s"],
            "%.2f" % u["user_s"],
            "%.2f" % u["sys_s"],
            "%.3f" % (u["user_s"] / u["uncapped_wall_s"]),
            "%.0f" % (u["max_rss_bytes"] / 2 ** 20),
        ])
    ratios = [float(r[4]) for r in rows]
    return table(
        ["fixture", "real s", "user s", "sys s", "user/real", "max RSS MB"], rows
    ) + (
        f"\nuser/real range {min(ratios):.3f}–{max(ratios):.3f} "
        f"(mean {sum(ratios) / len(ratios):.3f}) on a single router thread."
    )


# --------------------------------------------------------------------------
# Section 1b: hermetic uncapped re-run (2026-09-05)
# --------------------------------------------------------------------------


def section_uncapped_hermetic() -> str:
    """Old (contaminated, fanout/optimizer off) vs new (hermetic) uncapped runs.

    New data: bench/results/java-uncapped/<stem>.log +
    results.json (`java_settings.stages_ran`). Old data: the archive in
    UNCAPPED_DIR. The fanout/optimizer flags come from the stage lines the new
    pipeline actually printed, not from CLI intent.
    """
    old = fixture_manifest.load_uncapped(UNCAPPED_DIR)
    new = fixture_manifest.load_uncapped(HERMETIC_UNCAPPED_DIR)
    settings = {}
    res_path = os.path.join(HERMETIC_UNCAPPED_DIR, "results.json")
    if os.path.isfile(res_path):
        _meta, recs = fixture_manifest.load_results(res_path)
        settings = {
            r["fixture"]: (r.get("java_settings") or {}).get("stages_ran") or {}
            for r in recs
        }
    rows = []
    for name in sorted(new):
        n = new[name]
        o = old.get(name) or {}
        st = settings.get(name) or {}
        rows.append([
            name.replace(".dsn", ""),
            _fmt(o.get("uncapped_wall_s")), o.get("status", "-"),
            _fmt(o.get("uncapped_unrouted"), "%s"),
            _fmt(o.get("uncapped_violations"), "%s"),
            _fmt(n["uncapped_wall_s"]), n["status"],
            _fmt(n["uncapped_unrouted"], "%s"),
            _fmt(n["uncapped_violations"], "%s"),
            ("Y" if st.get("fanout") else "n") + "/" +
            ("Y" if st.get("optimizer") else "n"),
        ])
    completed = [u for u in new.values() if u["status"] == "completed"]
    summary = f"\n{len(completed)}/{len(new)} completed in the hermetic re-run."
    if completed:
        summary += (
            f" Completed wall sum "
            f"{sum(u['uncapped_wall_s'] for u in completed):.2f} s, max "
            f"{max(u['uncapped_wall_s'] for u in completed):.2f} s."
        )
    summary += (" Walls are W=2-contended upper bounds; status and u/v are "
                "concurrency-independent.")
    return table(
        ["fixture", "old wall s", "old status", "old u", "old v",
         "new wall s", "new status", "new u", "new v", "new fan/opt"],
        rows,
    ) + summary


# --------------------------------------------------------------------------
# Section 2: SerialGC
# --------------------------------------------------------------------------


def _experiment(stem: str, variant: str) -> dict:
    path = os.path.join(EXPERIMENTS_DIR, f"{stem}.{variant}.log")
    text = read(path)
    rec = fixture_manifest.parse_uncapped_log(text)
    rec["alloc_gb"] = sum(float(g) for g in ALLOC_GB.findall(text))
    rec["heap_mb"] = max(
        [float(m) for m in re.findall(r"([\d.]+) MB peak heap usage", text)] or [0]
    )
    rec["cpu_s"] = sum(
        float(m) for m in re.findall(r"([\d.]+) total CPU seconds", text)
    )
    stamps = FIRST_TS.findall(text)
    rec["started"], rec["ended"] = (stamps[0], stamps[-1]) if stamps else (None, None)
    return rec


def _stage(stages: dict, name: str):
    """One stage's seconds, tolerating a run in which the stage never ran."""
    return stages.get(name)


def section_serialgc() -> str:
    """Baseline vs SerialGC vs DEBUG-console, auto-routing stage only.

    Stage *sums* are not comparable across the two campaigns: the baseline ran
    a fanout stage on four of these five boards and the 05:03+ campaign ran it
    on none, so only the auto-routing stage measures the same work — and even
    that starts from a different board wherever fanout fired.
    """
    base = baseline_java()
    overlaps = _uncapped_overlaps()
    rows = []
    for stem in SERIALGC_FIXTURES:
        b = base[stem + ".dsn"]
        s = _experiment(stem, "serialgc")
        p = _experiment(stem, "profile")
        b_auto = _stage(b["stages"], next(
            k for k in b["stages"] if k.endswith("auto-routing")
        ))
        b_fan = next(
            (v for k, v in b["stages"].items() if k.endswith("fanout")), None
        )
        s_auto = _stage(s["stages"], "auto-routing")
        p_auto = _stage(p["stages"], "auto-routing")
        same = (
            b["unrouted"] == s["uncapped_unrouted"]
            and b["violations"] == s["uncapped_violations"]
            and b["passes_run"] == s["passes_run"]
        )
        b_alloc = sum(float(g) for g in ALLOC_GB.findall(
            read(os.path.join(BASELINE_DIR, "logs", f"{stem}.java.stdout.txt"))
        ))
        rows.append([
            stem,
            _fmt(b_fan, "%.2f", "(none)"),
            "%.2f" % b_auto,
            "%.2f" % s_auto,
            "%.2f" % p_auto,
            "%+.1f%%" % (100 * (s_auto - p_auto) / p_auto),
            "%+.1f%%" % (100 * (s_auto - b_auto) / b_auto),
            "%.0f / %.0f" % (b["peak_heap_bytes"] / 2 ** 20, s["heap_mb"]),
            "%.0f / %.0f" % (b_alloc, s["alloc_gb"]),
            "yes" if same else "**no**",
            len(overlaps[f"{stem}.serialgc"]),
        ])
    return table(
        ["fixture", "baseline fanout s", "baseline auto-route s",
         "SerialGC auto-route s", "DEBUG-console auto-route s",
         "SerialGC vs DEBUG-console", "SerialGC vs baseline",
         "peak heap MB base/serial", "alloc GB base/serial",
         "same outcome as baseline", "concurrent uncapped workers"],
        rows,
    )


def _uncapped_windows():
    """[(start, end, name, status)] per uncapped run, from its log timestamps.

    `blocked` runs are excluded by callers: they sat on a modal dialog at ~0 %
    CPU, so they were on the process table but not on the machine.
    """
    out = []
    for path in sorted(os.listdir(UNCAPPED_DIR)):
        if not path.endswith(".log"):
            continue
        text = read(os.path.join(UNCAPPED_DIR, path))
        stamps = FIRST_TS.findall(text)
        rec = fixture_manifest.parse_uncapped_log(text)
        if not stamps:
            continue
        # A run's log stops at its last INFO line; for the ones the cap killed
        # that is far short of the kill, so extend by the measured wall.
        out.append((stamps[0], stamps[-1], path[:-4], rec["status"]))
    return out


def _uncapped_overlaps() -> dict:
    """{'<stem>.<variant>': [uncapped run names live during it]}."""
    windows = _uncapped_windows()
    out = {}
    for stem in SERIALGC_FIXTURES:
        for variant in ("serialgc", "profile"):
            e = _experiment(stem, variant)
            out[f"{stem}.{variant}"] = sorted(
                w[2] for w in windows
                if w[0] <= e["ended"] and w[1] >= e["started"]
                and w[3] != "blocked"
            )
    return out


def section_uncapped_overlap() -> str:
    """How many other live JVMs each uncapped run shared the machine with.

    The log's last INFO line is not the process end for the runs the cap
    killed, so the window is [first timestamp, first timestamp + measured
    wall]; `blocked` runs are excluded as ~0 % CPU.
    """
    import datetime

    runs = []
    for path in sorted(os.listdir(UNCAPPED_DIR)):
        if not path.endswith(".log"):
            continue
        text = read(os.path.join(UNCAPPED_DIR, path))
        stamps = FIRST_TS.findall(text)
        rec = fixture_manifest.parse_uncapped_log(text)
        if not stamps or rec["uncapped_wall_s"] is None:
            continue
        start = datetime.datetime.strptime(stamps[0], "%Y-%m-%d %H:%M:%S")
        runs.append((
            path[:-4], start,
            start + datetime.timedelta(seconds=rec["uncapped_wall_s"]),
            rec["status"],
        ))
    rows = []
    for name, start, end, status in runs:
        overlap = [
            o[0] for o in runs
            if o[0] != name and o[3] != "blocked" and o[1] < end and o[2] > start
        ]
        rows.append([
            name, start.strftime("%H:%M:%S"), end.strftime("%H:%M:%S"),
            status, len(overlap),
        ])
    live = [r[4] for r in rows if r[3] != "blocked"]
    return table(
        ["run", "start", "end (start + wall)", "status", "overlapping live runs"],
        rows,
    ) + f"\noverlap over non-blocked runs: {min(live)}–{max(live)} other JVMs."


def section_concurrency() -> str:
    """Who was on the machine while each experiment ran."""
    overlaps = _uncapped_overlaps()
    rows = []
    for stem in SERIALGC_FIXTURES:
        for variant in ("serialgc", "profile"):
            e = _experiment(stem, variant)
            names = overlaps[f"{stem}.{variant}"]
            rows.append([
                f"{stem}.{variant}",
                e["started"][11:], e["ended"][11:],
                len(names),
                ", ".join(names) or "(none)",
            ])
    return table(
        ["run", "start", "end", "live uncapped workers", "which"], rows
    )


# --------------------------------------------------------------------------
# Section 3: Issue219
# --------------------------------------------------------------------------


def section_issue219() -> str:
    fanout_log = os.path.join(
        BASELINE_DIR, "logs", "Issue219-LogicBoard_smt.java.stdout.txt"
    )
    text = read(fanout_log)
    passes = [
        {
            "n": int(m.group(1)), "board": m.group(2), "s": float(m.group(3)),
            "fanouted": int(m.group(4)), "not_routed": int(m.group(5)),
            "vias": int(m.group(7)),
        }
        for m in FANOUT_PASS.finditer(text)
    ]
    stage = FANOUT_STAGE.search(text)
    stage_s = float(stage.group(2))
    tail = []
    for p in reversed(passes):
        if p["vias"] == 0:
            tail.append(p)
        else:
            break
    tail.reverse()
    tail_s = sum(p["s"] for p in tail)
    # "+extra vias" is the only progress signal the fanout loop prints. Split
    # the passes at 95% of the total: the head is the work that mattered.
    total_vias = sum(p["vias"] for p in passes)
    acc, cut = 0, len(passes)
    for i, p in enumerate(passes):
        acc += p["vias"]
        if acc >= 0.95 * total_vias:
            cut = i + 1
            break
    head, late = passes[:cut], passes[cut:]
    head_s = sum(p["s"] for p in head)
    late_s = sum(p["s"] for p in late)
    late_vias = sum(p["vias"] for p in late)

    rows = [
        [p["n"], "%.2f" % p["s"], p["fanouted"], p["not_routed"], "+%d" % p["vias"],
         p["board"][:8]]
        for p in passes
    ]
    unc = fixture_manifest.parse_uncapped_log(
        read(os.path.join(UNCAPPED_DIR, "Issue219-LogicBoard_smt.log"))
    )
    ar = list(fixture_manifest.RE_UNCAPPED_PASS.finditer(
        read(os.path.join(UNCAPPED_DIR, "Issue219-LogicBoard_smt.log"))
    ))
    # Java's own stop rule: the trailing run of passes that failed to beat the
    # running best score by more than 0.5. Reproduced here so the "wasted
    # passes" figure is the router's definition, not ours.
    best = None
    flat = []
    for m in ar:
        score = float(m.group(3).replace(",", "."))
        if best is not None and score <= best + 0.5:
            flat.append((int(m.group(1)), float(m.group(2)), score))
        else:
            best = score if best is None else max(best, score)
            flat = []
    flat_s = sum(f[1] for f in flat)

    return "\n".join([
        table(["fanout pass", "s", "SMD pins fanouted", "not routed",
               "extra vias", "board hash"], rows),
        "",
        f"fanout stage: {stage.group(1)} SMD pins in, {stage_s:.2f} s, "
        f"escaped {stage.group(3)}/{stage.group(4)}; "
        f"pass time sums to {sum(p['s'] for p in passes):.2f} s.",
        f"contiguous +0-via tail: passes #{tail[0]['n']}-#{tail[-1]['n']} "
        f"({len(tail)} of {len(passes)}), {tail_s:.2f} s = "
        f"{100 * tail_s / stage_s:.1f}% of the stage, "
        f"re-fanouting {min(p['fanouted'] for p in tail)}-"
        f"{max(p['fanouted'] for p in tail)} pins per pass, "
        f"a fresh board hash every pass.",
        f"passes #{head[0]['n']}-#{head[-1]['n']} place "
        f"{total_vias - late_vias} of the {total_vias} extra vias "
        f"({100 * (total_vias - late_vias) / total_vias:.1f}%) in "
        f"{head_s:.2f} s; the remaining {len(late)} passes spend "
        f"{late_s:.2f} s ({100 * late_s / stage_s:.1f}% of the stage) "
        f"for the other {late_vias}.",
        "",
        f"uncapped auto-routing tail (score frozen within 0.5): "
        f"passes #{flat[0][0]}-#{flat[-1][0]}, {flat_s:.2f} s = "
        f"{100 * flat_s / unc['routing_s']:.1f}% of the "
        f"{unc['routing_s']:.2f} s stage; the auto-router's stall detector "
        f"then fired.",
    ])


def section_stability() -> str:
    """§6: the 3-run sweep, one row per class and the boards in each."""
    stability = fixture_manifest.load_stability(STABILITY_DIR)
    if not stability:
        return "(no stability logs in " + fixture_manifest.repo_rel(STABILITY_DIR) + ")"
    by_class: dict = {}
    for name, block in sorted(stability.items()):
        by_class.setdefault(block["class"], []).append((name, block))

    rows = []
    for cls in fixture_manifest.STABILITY_CLASSES:
        members = by_class.get(cls, [])
        if not members:
            continue
        example = members[0][1]["reason"]
        rows.append([
            cls,
            len(members),
            example,
            ", ".join(n.replace(".dsn", "") for n, _ in members[:4])
            + (" …" if len(members) > 4 else ""),
        ])

    unstable = by_class.get("unstable", [])
    detail = "\n".join(
        f"- `{name}` — {block['reason']}" for name, block in unstable
    )
    summary = (
        f"\n{len(stability)} of 46 fixtures swept. "
        f"unstable: {len(unstable)}."
        + (f"\n\n{detail}" if detail else
           " Every fixture that reached a final score reached the same one all "
           "three times.")
    )
    return table(["class", "boards", "example reason", "members"], rows) + summary


SECTIONS = {
    "uncapped": section_uncapped,
    "uncapped-hermetic": section_uncapped_hermetic,
    "stability": section_stability,
    "uncapped-overlap": section_uncapped_overlap,
    "gc-surplus": section_gc_surplus,
    "serialgc": section_serialgc,
    "concurrency": section_concurrency,
    "issue219": section_issue219,
}


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--section", choices=sorted(SECTIONS), action="append")
    args = ap.parse_args(argv)
    for name in args.section or list(SECTIONS):
        print(f"### {name}\n")
        print(SECTIONS[name]())
        print()
    return 0


if __name__ == "__main__":
    sys.exit(main())
