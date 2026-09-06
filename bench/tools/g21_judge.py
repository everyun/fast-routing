#!/usr/bin/env python3
"""ROADMAP G2.1: judge one run of ours against the reference, per fixture.

This is the gate. `bench/README.md` ("Judging a session") describes the method
in prose; this file is the only implementation of it, and the two must be
changed together. It calls the `g21-referee` binary
(`bench/tools/g21_referee/`) three times per board — the input, our session,
the reference session — so both tools are measured with one checker on one
importer configuration, and it reads `bench/fixtures-manifest.json` for the
classification that decides which gate a board gets.

Usage (from the repo root):

    cargo build --release -p g21-referee
    python3 bench/tools/g21_judge.py --ours <dir-of-*.rust.ses>

    --java <dir>     reference sessions, `<stem>.java.ses`
                     (default: the checked-in baseline run)
    --sweep <json>   our runner's per-fixture record: wall_s, timed_out,
                     stop_reason, self_referee. Optional; without it the
                     report has no timing columns and a session file is
                     trusted on sight.
    --cache <json>   referee verdicts, reused when the inputs have not changed
    --out <json>     the per-fixture verdict table (default: <ours>/../gate.json)

# The two gates

A board is judged **comparatively** — against what the reference actually did —
only when the reference's answer is a fact that reproduces:

    java.class == "java_finished"  AND  stability.class == "stable"
    AND a reference session exists for it

    pass iff  added_violations(ours) <= added_violations(java)
         AND  unrouted(ours)         <= unrouted(java)

`added` is always `judged − the same referee on the imported input board`, and
a violation between two `USER_FIXED` items is never charged to either tool: it
is a property of the input.

Everything else — timeout, crash, degenerate, *unstable*, and any board whose
reference session is missing — is judged **absolutely**:

    pass iff  added_violations(ours) == 0
         AND  unrouted(ours) <= unrouted(input)
         AND  (no reference session OR unrouted(ours) <= unrouted(java))
         AND  the anti-degeneracy guard below

## Why `unrouted <= input` and not `< input`

The gate used to demand strict progress on every absolute board. `Issue229`
is the counter-example that broke it. The board arrives with 20 of its 64
connections open; every one of them is blocked by `USER_FIXED` copper that no
router is allowed to move, and upstream cannot attach to it either. Our result
is 20 unrouted — the input, unchanged, with zero added violations. The
reference's own result on that board is **64 unrouted**: its session drops the
input's routing and is 44 connections *worse* than doing nothing. A gate that
fails us for tying the input while the tool we are being compared against
regresses it is measuring the wrong thing, so the clause is `<=` plus the
explicit "and no worse than the reference" line above.

## The anti-degeneracy guard, and why `<=` is still not cheatable

`<=` on its own would let "emit an empty session" pass every absolute board.
It cannot, because a board is only allowed to stand still when *the reference
implementation could not move it either*. Progress is a recorded property of
the reference, never of the run being judged:

  1. a referee-judged reference session that beats the input, or
  2. a stability run whose own `unrouted` beat its own `started with N
     unrouted` (Java's numbers compared with Java's numbers, never with the
     referee's), or
  3. the uncapped campaign: either the same self-reported improvement, or the
     `hit_cap`-with-zero-passes case — 2 h of routing that never finished a
     single pass is positive evidence that the board did not move, not missing
     data.

If any of those says the reference made progress, we must make progress too:
the session has to carry copper for at least one multi-pin net, *and*
`unrouted(ours) < unrouted(input)` strictly. If they all say it did not, the
board is at its routable fixpoint as far as any evidence goes, and standing
still is the correct answer. If there is no evidence at all — no reference
session, no stability run, no uncapped log — the guard assumes progress is
possible and demands it: silence must never be a way through.

The one board the guard never applies to is one that arrives with nothing
unrouted (`Issue103-Board-Routed` and friends). There is no progress to make,
`unrouted(ours) == 0 == input` is the perfect result, and demanding `< input`
of it would be unsatisfiable.

`Issue208` is the case that exercises the "no progress" branch. Neither tool
routes it: the reference spent 7200 s in the routing stage without finishing
pass 1 (`uncapped.passes_run == 0`), and all three stability runs were killed
at the cap having reported nothing. We emit a well-formed, copper-empty
session in 66 s that adds no violations and loses no connection, and that is
the whole of what the board permits today.

## A session that does not read back is a failure

Every board additionally requires `ses_errors == 0` from the referee's read.
A session the reader has to guess at is not a result, however good its
numbers look.

Stdlib Python 3 only, like the rest of `bench/`.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from collections import Counter

BENCH_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REPO_ROOT = os.path.dirname(BENCH_DIR)

DEFAULT_FIXTURES = "/tmp/freerouting-upstream/fixtures"
DEFAULT_MANIFEST = os.path.join(BENCH_DIR, "fixtures-manifest.json")
DEFAULT_JAVA_SES = os.path.join(BENCH_DIR, "results", "java-reference", "ses")
DEFAULT_REFEREE = os.path.join(REPO_ROOT, "target", "release", "g21-referee")

# `java.class` values that describe a reference run whose result is a number.
# Everything else is "the reference did not produce one".
COMPARABLE_JAVA_CLASSES = {"java_finished"}
# The one `stability.class` that makes such a number reproducible.
COMPARABLE_STABILITY = "stable"


# --------------------------------------------------------------------------
# referee invocation
# --------------------------------------------------------------------------


def run_referee(referee: str, dsn: str, ses: str | None, timeout: float) -> dict:
    cmd = [referee, dsn] + ([ses] if ses else [])
    try:
        done = subprocess.run(cmd, capture_output=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return {"error": f"referee timed out after {timeout:.0f}s"}
    text = done.stdout.decode("utf-8", "replace")
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return {"error": (text + done.stderr.decode("utf-8", "replace"))[:300]}


def stamp(path: str) -> str:
    try:
        st = os.stat(path)
        return f"{path}:{st.st_size}:{int(st.st_mtime)}"
    except OSError:
        return f"{path}:missing"


class Judged:
    """Referee verdicts, memoised on (path, size, mtime) of the inputs.

    The referee binary itself is part of the key: rebuilding it is exactly the
    situation where a stale verdict would be a lie, and the checker changes far
    more often than the fixtures do.
    """

    def __init__(self, referee: str, path: str | None, timeout: float):
        self.referee = referee
        self.referee_stamp = stamp(referee)
        self.path = path
        self.timeout = timeout
        self.entries: dict = {}
        if path and os.path.exists(path):
            with open(path, "r", encoding="utf-8") as f:
                self.entries = json.load(f)

    def get(self, dsn: str, ses: str | None) -> dict:
        key = f"{self.referee_stamp}|{stamp(dsn)}|{stamp(ses) if ses else '-'}"
        if key not in self.entries:
            self.entries[key] = run_referee(self.referee, dsn, ses, self.timeout)
        return self.entries[key]

    def save(self):
        if not self.path:
            return
        with open(self.path, "w", encoding="utf-8") as f:
            json.dump(self.entries, f, indent=1)


# --------------------------------------------------------------------------
# the gate
# --------------------------------------------------------------------------


def reference_made_progress(entry: dict, base: dict, java: dict | None):
    """Did the *reference implementation* ever improve this board?

    Returns True / False / None (no evidence either way). See the module docs;
    the ordering is by how directly the evidence answers the question, and the
    two self-reported sources are only ever compared with themselves.
    """
    if java and "error" not in java:
        return java["incomplete"] < base["incomplete"], "reference session vs input"

    stability = entry.get("stability") or {}
    started = stability.get("started_unrouted")
    runs = list((stability.get("per_run") or {}).values())
    scored = [r for r in runs if r.get("unrouted") is not None]
    if started is not None and scored:
        return (
            any(r["unrouted"] < started for r in scored),
            "stability runs vs their own starting count",
        )

    uncapped = entry.get("uncapped") or {}
    if uncapped:
        final = uncapped.get("uncapped_unrouted")
        started = uncapped.get("started_unrouted")
        if final is not None and started is not None:
            return final < started, "uncapped run vs its own starting count"
        if uncapped.get("status") == "hit_cap" and uncapped.get("passes_run") == 0:
            return (
                False,
                f"uncapped run spent {uncapped.get('uncapped_wall_s', 0):.0f}s in the "
                f"routing stage without finishing pass 1",
            )

    if runs and not scored and any(r.get("routing_started") for r in runs):
        return False, "every stability run was killed at the cap with nothing reported"

    return None, "no reference evidence for this board"


def judge_one(name: str, entry: dict, base, ours, java, run) -> dict:
    """One fixture's row, with `verdict` and a `reason` that names the number."""
    row = {
        "fixture": name,
        "java_class": entry["java"]["class"],
        "stability_class": (entry.get("stability") or {}).get("class", "unmeasured"),
        "base_unrouted": num(base, "incomplete"),
        "base_violations": num(base, "violations_charged"),
        "ours_unrouted": num(ours, "incomplete"),
        "ours_violations": num(ours, "violations_charged"),
        "ours_violations_raw": num(ours, "violations"),
        "ours_both_user_fixed": num(ours, "violations_both_user_fixed"),
        "ours_ses_errors": num(ours, "ses_errors"),
        "ours_nets_with_copper": num(ours, "nets_with_copper"),
        "java_unrouted": num(java, "incomplete"),
        "java_violations": num(java, "violations_charged"),
        "dsn_multi_pin_nets": num(base, "nets_multi_pin"),
        "overlay_carried_over": num(ours, "overlay_carried_over"),
        "wall_s": run.get("wall_s"),
        "timed_out": run.get("timed_out"),
        "stop_reason": run.get("stop_reason"),
        "doors_mode": run.get("doors_mode"),
        "passes_run": run.get("passes_run"),
        "rss_mb": round((run.get("max_rss_bytes") or 0) / 1e6, 1) or None,
        "self_referee": run.get("self_referee"),
    }

    # A file that is not a Specctra board at all (Issue006 is a renamed binary
    # compound document). Neither tool has anything to do; nothing to gate.
    if base is None or "error" in base:
        return finish(row, "pass", "-", "input is not a routable board; both tools no-op")

    if ours is None or "error" in ours:
        reason = (
            "timed out with no session written"
            if run.get("timed_out")
            else ours.get("error", "no session produced") if ours else "no session produced"
        )
        return finish(row, "FAIL", "-", reason)

    if row["ours_ses_errors"]:
        return finish(
            row, "FAIL", "-",
            f"our session does not read back cleanly ({row['ours_ses_errors']} reader errors)",
        )

    added = row["ours_violations"] - row["base_violations"]
    row["added_violations"] = added

    comparative = (
        entry["java"]["class"] in COMPARABLE_JAVA_CLASSES
        and row["stability_class"] == COMPARABLE_STABILITY
        and java is not None
        and "error" not in java
    )

    if comparative:
        java_added = row["java_violations"] - row["base_violations"]
        row["java_added_violations"] = java_added
        problems = []
        if added > java_added:
            problems.append(f"added {added} violations, reference added {java_added}")
        if row["ours_unrouted"] > row["java_unrouted"]:
            problems.append(
                f"unrouted {row['ours_unrouted']} > reference {row['java_unrouted']}"
            )
        return finish(
            row,
            "FAIL" if problems else "pass",
            "comparative",
            "; ".join(problems) or "comparative gate met",
        )

    # --- absolute gate ---------------------------------------------------
    progress, evidence = reference_made_progress(entry, base, java)
    row["reference_made_progress"] = progress
    row["progress_evidence"] = evidence

    problems = []
    if added != 0:
        problems.append(f"added {added} violations")
    if row["ours_unrouted"] > row["base_unrouted"]:
        problems.append(
            f"unrouted {row['ours_unrouted']} above the input's {row['base_unrouted']}"
        )
    if java is not None and "error" not in java and row["ours_unrouted"] > row["java_unrouted"]:
        problems.append(
            f"unrouted {row['ours_unrouted']} > reference {row['java_unrouted']}"
        )

    # Anti-degeneracy: only a board the reference could not move is allowed to
    # come back unchanged. `None` (no evidence) is treated as "it could".
    # A board that arrives with nothing unrouted has no progress to make, and
    # `unrouted(ours) == 0 == input` is the perfect result, not a degenerate one.
    if progress is not False and row["base_unrouted"] > 0:
        if row["dsn_multi_pin_nets"] and not row["ours_nets_with_copper"]:
            problems.append(
                "anti-degeneracy: the session carries no copper for any multi-pin net"
                f" ({evidence})"
            )
        elif row["ours_unrouted"] >= row["base_unrouted"]:
            problems.append(
                f"anti-degeneracy: no progress ({row['ours_unrouted']} unrouted, input "
                f"{row['base_unrouted']}) on a board the reference moved ({evidence})"
            )

    note = ""
    if entry["java"]["class"] in COMPARABLE_JAVA_CLASSES:
        note = (
            f" (java_finished but stability is {row['stability_class']})"
            if row["stability_class"] != COMPARABLE_STABILITY
            else " (java_finished but its session is absent from the reference run)"
        )
    return finish(
        row,
        "FAIL" if problems else "pass",
        "absolute",
        ("; ".join(problems) or "absolute gate met") + note,
    )


def finish(row: dict, verdict: str, gate: str, reason: str) -> dict:
    row["gate"] = gate
    row["verdict"] = verdict
    row["reason"] = reason
    return row


def num(verdict, key):
    if not verdict or "error" in verdict:
        return None
    return verdict.get(key)


# --------------------------------------------------------------------------
# reporting
# --------------------------------------------------------------------------


def fidelity_gaps(rows) -> list:
    """Boards where the re-imported session is judged differently from the board
    the router had in memory.

    `--sweep`'s `self_referee` is our CLI's own line, printed off the live board
    before it was written out; the judged numbers come from reading that file
    back. A gap means the session file, the writer or the overlay lost or
    invented something — it is the alarm that found the `(type fix)` overlay bug
    (`fr_io::specctra::overlay`). The raw violation count is the comparable one,
    because the CLI does not know about the charged/uncharged split.
    """
    gaps = []
    for row in rows:
        line = row.get("self_referee")
        if not line or row["ours_unrouted"] is None:
            continue
        m = re.search(r"(\d+) clearance violations, (\d+) of (\d+)", line)
        if not m:
            continue
        violations, unrouted = int(m.group(1)), int(m.group(2))
        if violations != row["ours_violations_raw"] or unrouted != row["ours_unrouted"]:
            gaps.append(
                (
                    row["fixture"],
                    f"in process {unrouted}u/{violations}v",
                    f"re-imported {row['ours_unrouted']}u/{row['ours_violations_raw']}v",
                )
            )
    return gaps


def report(rows, stream=sys.stdout):
    def cell(unrouted, violations):
        return "n/a" if unrouted is None else f"{unrouted}u/{violations}v"

    modes = {r.get("doors_mode") for r in rows if r.get("doors_mode")}
    mode_str = f" [doors_mode={', '.join(sorted(modes))}]" if modes else ""

    header = (
        f"{'fixture':44s} {'class':15s} {'stability':11s} {'gate':12s} "
        f"{'input':>9s} {'ours':>9s} {'reference':>9s} verdict  reason{mode_str}"
    )
    print(header, file=stream)
    for row in rows:
        print(
            f"{row['fixture'][:42]:44s} {row['java_class']:15s} "
            f"{row['stability_class']:11s} {row['gate']:12s} "
            f"{cell(row['base_unrouted'], row['base_violations']):>9s} "
            f"{cell(row['ours_unrouted'], row['ours_violations']):>9s} "
            f"{cell(row['java_unrouted'], row['java_violations']):>9s} "
            f"{row['verdict']:8s} {row['reason']}",
            file=stream,
        )

    gaps = fidelity_gaps(rows)
    print(
        f"\nSES round-trip fidelity: {len(gaps)} board(s) judged differently from the "
        f"in-memory board",
        file=stream,
    )
    for gap in gaps:
        print(f"    {gap[0]}: {gap[1]} vs {gap[2]}", file=stream)

    print("\nper class:", file=stream)
    total = Counter()
    passed = Counter()
    for row in rows:
        key = f"{row['java_class']}/{row['gate']}"
        total[key] += 1
        if row["verdict"] == "pass":
            passed[key] += 1
    for key in sorted(total):
        print(f"  {key:32s} {passed[key]:2d}/{total[key]:2d} pass", file=stream)
    print(
        f"  {'TOTAL':32s} {sum(passed.values()):2d}/{sum(total.values()):2d} pass",
        file=stream,
    )


# --------------------------------------------------------------------------


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--ours", required=True,
                    help="directory of our sessions, named <stem>.rust.ses")
    ap.add_argument("--java", default=DEFAULT_JAVA_SES,
                    help="directory of reference sessions, named <stem>.java.ses")
    ap.add_argument("--fixtures", default=DEFAULT_FIXTURES)
    ap.add_argument("--manifest", default=DEFAULT_MANIFEST)
    ap.add_argument("--referee", default=DEFAULT_REFEREE,
                    help="the g21-referee binary "
                         "(cargo build --release -p g21-referee)")
    ap.add_argument("--sweep", default=None,
                    help="our runner's sweep.json, for the timing columns and "
                         "for refusing a session left behind by a killed run")
    ap.add_argument("--cache", default=None, help="referee verdict cache (json)")
    ap.add_argument("--out", default=None, help="where to write the verdict table")
    ap.add_argument("--referee-timeout", type=float, default=3600.0)
    args = ap.parse_args(argv)

    if not os.path.exists(args.referee):
        print(f"error: no referee binary at {args.referee}; run "
              f"`cargo build --release -p g21-referee`", file=sys.stderr)
        return 2

    with open(args.manifest, "r", encoding="utf-8") as f:
        manifest = json.load(f)
    entries = {e["name"]: e for e in manifest["fixtures"]}

    runs: dict = {}
    if args.sweep:
        with open(args.sweep, "r", encoding="utf-8") as f:
            for record in json.load(f):
                runs[record["fixture"]] = record

    judged = Judged(args.referee, args.cache, args.referee_timeout)

    rows = []
    for name in sorted(entries):
        entry = entries[name]
        stem = name[: -len(".dsn")]
        dsn = os.path.join(args.fixtures, name)
        if not os.path.exists(dsn):
            print(f"error: fixture missing: {dsn} (run bench/fetch_fixtures.sh)",
                  file=sys.stderr)
            return 2
        run = runs.get(name, {})

        base = judged.get(dsn, None)

        ours_path = os.path.join(args.ours, stem + ".rust.ses")
        # A killed run can still leave a truncated file behind, so a session is
        # only trusted from a process that exited on its own.
        usable = os.path.exists(ours_path) and os.path.getsize(ours_path) > 0
        if run.get("timed_out"):
            usable = False
        ours = judged.get(dsn, ours_path) if usable else None

        java_path = os.path.join(args.java, stem + ".java.ses")
        java = (
            judged.get(dsn, java_path)
            if os.path.exists(java_path) and os.path.getsize(java_path) > 0
            else None
        )

        rows.append(judge_one(name, entry, base, ours, java, run))

    judged.save()
    report(rows)

    out = args.out or os.path.join(os.path.dirname(os.path.abspath(args.ours)), "gate.json")
    with open(out, "w", encoding="utf-8") as f:
        json.dump(rows, f, indent=1, ensure_ascii=False)
    print(f"\nwrote {out}")

    failures = sum(1 for row in rows if row["verdict"] != "pass")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
