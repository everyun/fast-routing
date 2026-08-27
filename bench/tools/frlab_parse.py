#!/usr/bin/env python3
"""Parser and differ for the instrumented lab jar's `FRLAB` output.

The lab jar (`/tmp/fr-lab-release/freerouting-lab.jar`, built from the release
revision `2d4de019` and proven non-perturbing — see `docs/ROADMAP.md` M0.5)
emits three record types into its normal log stream:

    FRLAB expansions_pass=<N> pass=<P>
    FRLAB expansions_conn n=<N> p50=<N> p99=<N> max=<N> pass=<P>
    FRLAB settle <door_id> <section_no> <expansion_value> <sorting_value>

The first two are always on; the settle trace needs `-Dfrlab.settle_trace=1`.
Our own router emits the same settle record with its own tool token,
`FRMETRIC settle …` (see `crates/fr-metrics/src/settle_trace.rs`), so one parser
and one diff serve both sides of the M2 differential harness.

This module is the core of that harness: `parse_log` turns a log into per-pass
records plus the settle sequence, and `diff_settles` reports the first index at
which two settle sequences part company, with a context window around it.

Stdlib only. See `bench/tools/README.md` for the format contract and for how M2
uses this.

Usage:

    python3 bench/tools/frlab_parse.py summary <log> [<log> …] [--json]
    python3 bench/tools/frlab_parse.py diff <ours.log> <java.log> [--window 5]
    python3 bench/tools/frlab_parse.py --self-test
"""

from __future__ import annotations

import argparse
import json
import math
import re
import sys
import unittest
from dataclasses import dataclass, field, replace
from typing import Iterable, Iterator, Sequence

# --------------------------------------------------------------------------
# Record grammar
# --------------------------------------------------------------------------
# The records are embedded in FRLogger output, so they never start the line:
#   2026-08-28 06:20:24.598 INFO   FRLAB settle 1086 0 0.0 231507.24242124692
# Anchoring on the tool token rather than the line start is deliberate.

RE_PASS = re.compile(r"\bFRLAB\s+expansions_pass=(\d+)\s+pass=(\d+)\b")
RE_CONN = re.compile(
    r"\bFRLAB\s+expansions_conn\s+n=(\d+)\s+p50=(\d+)\s+p99=(\d+)\s+max=(\d+)\s+pass=(\d+)\b"
)
# `FRLAB settle` (Java lab jar) and `FRMETRIC settle` (fr-metrics) are the same
# record; g and f are captured loosely because Java prints doubles as `0.0`,
# `1.0E7`, `Infinity` or `NaN` — all of which float() accepts.
RE_SETTLE = re.compile(
    r"\bFR(?:LAB|METRIC)\s+settle\s+(-?\d+)\s+(-?\d+)\s+(\S+)\s+(\S+)"
)
# fr-metrics' JSON report lines: `FRMETRIC {"kind":"counters", …}`.
RE_FRMETRIC_JSON = re.compile(r"\bFRMETRIC\s+(\{.*\})\s*$")


@dataclass(frozen=True)
class Settle:
    """One maze element popped off the frontier and expanded.

    `door_id` and `section` are the topological identity — they are integers
    derived from upstream's `getId()` tie-breaks and must match exactly. `g`
    (expansion value) and `f` (sorting value) are f64 and are compared with a
    tolerance: per ARCHITECTURE, f64 never decides topology, so a pure g/f drift
    is a weaker signal than an order swap.
    """

    index: int
    door_id: int
    section: int
    g: float
    f: float
    pass_no: int | None = None

    @property
    def key(self) -> tuple[int, int]:
        """The topological identity of the settle."""
        return (self.door_id, self.section)

    def __str__(self) -> str:
        return f"#{self.index} door={self.door_id} sec={self.section} g={self.g!r} f={self.f!r}"


@dataclass
class PassRecord:
    """Everything reported about one router pass."""

    pass_no: int
    expansions: int | None = None
    conn_n: int | None = None
    conn_p50: int | None = None
    conn_p99: int | None = None
    conn_max: int | None = None
    settles_seen: int = 0

    @property
    def consistent(self) -> bool:
        """True when the settle lines seen match the pass's own expansion total.

        A mismatch is meaningful, not cosmetic: either the trace was truncated,
        or settles happened outside the pass window (the fanout stage runs maze
        searches too), or `--limit` cut the parse short.
        """
        return self.expansions is None or self.expansions == self.settles_seen

    def to_dict(self) -> dict:
        return {
            "pass": self.pass_no,
            "expansions": self.expansions,
            "conn_n": self.conn_n,
            "conn_p50": self.conn_p50,
            "conn_p99": self.conn_p99,
            "conn_max": self.conn_max,
            "settles_seen": self.settles_seen,
            "consistent": self.consistent,
        }


@dataclass
class LabLog:
    """A parsed log: per-pass records, the settle sequence, FRMETRIC objects."""

    source: str = "<memory>"
    passes: list[PassRecord] = field(default_factory=list)
    settles: list[Settle] = field(default_factory=list)
    frmetric: list[dict] = field(default_factory=list)
    truncated: bool = False

    @property
    def total_expansions(self) -> int:
        """Sum of the per-pass `expansions_pass` totals."""
        return sum(record.expansions or 0 for record in self.passes)

    @property
    def unattributed_settles(self) -> int:
        """Settles with no pass line after them (an interrupted or partial log)."""
        return sum(1 for settle in self.settles if settle.pass_no is None)

    def pass_record(self, pass_no: int) -> PassRecord | None:
        for record in self.passes:
            if record.pass_no == pass_no:
                return record
        return None

    def settles_of_pass(self, pass_no: int) -> list[Settle]:
        return [settle for settle in self.settles if settle.pass_no == pass_no]

    def to_dict(self) -> dict:
        return {
            "source": self.source,
            "total_expansions": self.total_expansions,
            "settles": len(self.settles),
            "unattributed_settles": self.unattributed_settles,
            "truncated": self.truncated,
            "passes": [record.to_dict() for record in self.passes],
            "frmetric": self.frmetric,
        }


# --------------------------------------------------------------------------
# Parsing
# --------------------------------------------------------------------------


def parse_lines(lines: Iterable[str], source: str = "<memory>", max_settles: int | None = None) -> LabLog:
    """Parses an iterable of log lines into a `LabLog`.

    Settle records are attributed to the pass whose `expansions_pass` line
    follows them, because upstream logs that line at the *end* of the pass
    (`BatchAutorouter.autoroute_pass`). Settles with no following pass line keep
    `pass_no = None`.

    `max_settles` caps the settle sequence (large boards trace millions of
    lines); pass records are still parsed in full and the log is flagged
    `truncated`.
    """
    log = LabLog(source=source)
    by_pass: dict[int, PassRecord] = {}
    # Settles from `log.settles[pending_start:]` have not been attributed yet.
    pending_start = 0
    index = 0

    def record_for(pass_no: int) -> PassRecord:
        record = by_pass.get(pass_no)
        if record is None:
            record = PassRecord(pass_no=pass_no)
            by_pass[pass_no] = record
            log.passes.append(record)
        return record

    for line in lines:
        if "FRLAB" not in line and "FRMETRIC" not in line:
            continue

        match = RE_SETTLE.search(line)
        if match:
            if max_settles is not None and index >= max_settles:
                log.truncated = True
                index += 1
                continue
            settle = Settle(
                index=index,
                door_id=int(match.group(1)),
                section=int(match.group(2)),
                g=float(match.group(3)),
                f=float(match.group(4)),
            )
            log.settles.append(settle)
            index += 1
            continue

        match = RE_PASS.search(line)
        if match:
            pass_no = int(match.group(2))
            record = record_for(pass_no)
            record.expansions = int(match.group(1))
            # Settles logged since the previous pass line belong to this pass:
            # upstream logs `expansions_pass` at the *end* of the pass.
            attributed = log.settles[pending_start:]
            log.settles[pending_start:] = [
                replace(settle, pass_no=pass_no) for settle in attributed
            ]
            record.settles_seen += len(attributed)
            pending_start = len(log.settles)
            continue

        match = RE_CONN.search(line)
        if match:
            record = record_for(int(match.group(5)))
            record.conn_n = int(match.group(1))
            record.conn_p50 = int(match.group(2))
            record.conn_p99 = int(match.group(3))
            record.conn_max = int(match.group(4))
            continue

        match = RE_FRMETRIC_JSON.search(line)
        if match:
            try:
                log.frmetric.append(json.loads(match.group(1)))
            except json.JSONDecodeError:
                pass

    log.passes.sort(key=lambda record: record.pass_no)
    return log


def parse_log(path: str, max_settles: int | None = None) -> LabLog:
    """Parses a log file. Undecodable bytes are replaced, never fatal."""
    with open(path, "r", encoding="utf-8", errors="replace") as handle:
        return parse_lines(handle, source=path, max_settles=max_settles)


def iter_settles(path: str) -> Iterator[Settle]:
    """Streams just the settle records — for logs too large to hold in memory."""
    index = 0
    with open(path, "r", encoding="utf-8", errors="replace") as handle:
        for line in handle:
            match = RE_SETTLE.search(line)
            if not match:
                continue
            yield Settle(
                index=index,
                door_id=int(match.group(1)),
                section=int(match.group(2)),
                g=float(match.group(3)),
                f=float(match.group(4)),
            )
            index += 1


# --------------------------------------------------------------------------
# Differential harness
# --------------------------------------------------------------------------

IDENTICAL = "identical"
TOPOLOGY = "topology"
LENGTH = "length"
VALUE = "value"


@dataclass
class SettleDiff:
    """The result of comparing two settle sequences.

    `kind` is one of:

    - `identical` — same keys and, within tolerance, same g/f
    - `topology`  — the two runs settled a different `(door_id, section)` at
                    `first_divergence`; this is the finding that matters, the
                    routers took different paths from here on
    - `length`    — the keys agree over the whole shorter sequence but one run
                    stopped early (a stop rule fired, or a trace was truncated)
    - `value`     — same order, but g or f drifted beyond the tolerance at
                    `first_divergence`; a numeric-precision finding, not a
                    routing divergence
    """

    kind: str
    first_divergence: int | None
    matched_prefix: int
    left_len: int
    right_len: int
    left_name: str = "left"
    right_name: str = "right"
    context: list[tuple[int, Settle | None, Settle | None]] = field(default_factory=list)
    detail: str = ""

    @property
    def identical(self) -> bool:
        return self.kind == IDENTICAL

    def to_dict(self) -> dict:
        return {
            "kind": self.kind,
            "first_divergence": self.first_divergence,
            "matched_prefix": self.matched_prefix,
            "left": {"name": self.left_name, "settles": self.left_len},
            "right": {"name": self.right_name, "settles": self.right_len},
            "detail": self.detail,
        }

    def render(self) -> str:
        """A human-readable divergence report with the context window."""
        lines = [
            f"{self.left_name}: {self.left_len} settles",
            f"{self.right_name}: {self.right_len} settles",
            f"kind={self.kind} matched_prefix={self.matched_prefix} "
            f"first_divergence={self.first_divergence}",
        ]
        if self.detail:
            lines.append(self.detail)
        if self.context:
            lines.append("")
            lines.append(f"{'idx':>8}  {'':1}  {self.left_name:<38}  {self.right_name:<38}")
            for index, left, right in self.context:
                marker = ">" if index == self.first_divergence else " "
                lines.append(
                    f"{index:>8}  {marker}  {_cell(left):<38}  {_cell(right):<38}"
                )
        return "\n".join(lines)


def _cell(settle: Settle | None) -> str:
    if settle is None:
        return "-"
    return f"door={settle.door_id} sec={settle.section} g={settle.g:.6g} f={settle.f:.6g}"


def _close(left: float, right: float, tolerance: float) -> bool:
    if math.isnan(left) and math.isnan(right):
        return True
    return math.isclose(left, right, rel_tol=tolerance, abs_tol=tolerance)


def diff_settles(
    left: Sequence[Settle],
    right: Sequence[Settle],
    *,
    window: int = 5,
    tolerance: float = 1e-9,
    left_name: str = "left",
    right_name: str = "right",
) -> SettleDiff:
    """Finds the first index at which two settle sequences diverge.

    Topological divergence — a different `(door_id, section)` — is reported in
    preference to a g/f drift, and a g/f drift in preference to a clean length
    difference, because that is the order of severity for the M2 gate.
    """
    shorter = min(len(left), len(right))

    topology_at: int | None = None
    for index in range(shorter):
        if left[index].key != right[index].key:
            topology_at = index
            break

    if topology_at is not None:
        detail = (
            f"{left_name} settled {left[topology_at].key}, "
            f"{right_name} settled {right[topology_at].key}"
        )
        return SettleDiff(
            kind=TOPOLOGY,
            first_divergence=topology_at,
            matched_prefix=topology_at,
            left_len=len(left),
            right_len=len(right),
            left_name=left_name,
            right_name=right_name,
            context=_context(left, right, topology_at, window),
            detail=detail,
        )

    value_at: int | None = None
    for index in range(shorter):
        if not _close(left[index].g, right[index].g, tolerance) or not _close(
            left[index].f, right[index].f, tolerance
        ):
            value_at = index
            break

    if len(left) != len(right):
        # The keys agree everywhere both sequences have one; the shorter run
        # stopped first. Report that before any g/f drift after it.
        if value_at is None or value_at >= shorter:
            longer_name = left_name if len(left) > len(right) else right_name
            return SettleDiff(
                kind=LENGTH,
                first_divergence=shorter,
                matched_prefix=shorter,
                left_len=len(left),
                right_len=len(right),
                left_name=left_name,
                right_name=right_name,
                context=_context(left, right, shorter, window),
                detail=f"identical for {shorter} settles, then only {longer_name} continues",
            )

    if value_at is not None:
        detail = (
            f"g/f drift: {left_name} g={left[value_at].g!r} f={left[value_at].f!r}, "
            f"{right_name} g={right[value_at].g!r} f={right[value_at].f!r}"
        )
        return SettleDiff(
            kind=VALUE,
            first_divergence=value_at,
            matched_prefix=value_at,
            left_len=len(left),
            right_len=len(right),
            left_name=left_name,
            right_name=right_name,
            context=_context(left, right, value_at, window),
            detail=detail,
        )

    return SettleDiff(
        kind=IDENTICAL,
        first_divergence=None,
        matched_prefix=shorter,
        left_len=len(left),
        right_len=len(right),
        left_name=left_name,
        right_name=right_name,
        detail=f"{shorter} settles agree",
    )


def _context(
    left: Sequence[Settle],
    right: Sequence[Settle],
    center: int,
    window: int,
) -> list[tuple[int, Settle | None, Settle | None]]:
    """The `window` records either side of `center`, from both sequences."""
    start = max(0, center - window)
    stop = min(max(len(left), len(right)), center + window + 1)
    rows = []
    for index in range(start, stop):
        rows.append(
            (
                index,
                left[index] if index < len(left) else None,
                right[index] if index < len(right) else None,
            )
        )
    return rows


# --------------------------------------------------------------------------
# CLI
# --------------------------------------------------------------------------


def _summary_text(log: LabLog) -> str:
    lines = [
        f"{log.source}",
        f"  total expansions : {log.total_expansions}",
        f"  settle records   : {len(log.settles)}"
        + ("  (truncated by --limit)" if log.truncated else ""),
    ]
    if log.unattributed_settles:
        lines.append(f"  unattributed     : {log.unattributed_settles}")
    if log.passes:
        lines.append(f"  {'pass':>4} {'expansions':>11} {'n':>6} {'p50':>7} {'p99':>8} {'max':>8}  settles")
        for record in log.passes:
            flag = "" if record.consistent else "  MISMATCH"
            lines.append(
                f"  {record.pass_no:>4} {_num(record.expansions):>11} {_num(record.conn_n):>6} "
                f"{_num(record.conn_p50):>7} {_num(record.conn_p99):>8} {_num(record.conn_max):>8}"
                f"  {record.settles_seen}{flag}"
            )
    for entry in log.frmetric:
        lines.append(f"  FRMETRIC {entry.get('kind', '?')}: {json.dumps(entry, sort_keys=True)}")
    return "\n".join(lines)


def _num(value: int | None) -> str:
    return "-" if value is None else str(value)


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Parse and diff FRLAB / FRMETRIC settle traces.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    parser.add_argument("--self-test", action="store_true", help="run the built-in tests and exit")
    subparsers = parser.add_subparsers(dest="command")

    summary = subparsers.add_parser("summary", help="per-pass totals of one or more logs")
    summary.add_argument("logs", nargs="+")
    summary.add_argument("--json", action="store_true", help="emit JSON instead of a table")
    summary.add_argument("--limit", type=int, default=None, help="stop collecting settles after N")

    diff = subparsers.add_parser("diff", help="diff two settle sequences")
    diff.add_argument("left", help="our log")
    diff.add_argument("right", help="the lab jar's log (the reference)")
    diff.add_argument("--window", type=int, default=5, help="context records either side")
    diff.add_argument("--tolerance", type=float, default=1e-9, help="relative g/f tolerance")
    diff.add_argument("--limit", type=int, default=None, help="compare at most N settles")
    diff.add_argument("--json", action="store_true", help="emit JSON instead of a report")

    args = parser.parse_args(argv)

    if args.self_test:
        return _run_self_test()

    if args.command == "summary":
        for path in args.logs:
            log = parse_log(path, max_settles=args.limit)
            print(json.dumps(log.to_dict()) if args.json else _summary_text(log))
        return 0

    if args.command == "diff":
        left = parse_log(args.left, max_settles=args.limit)
        right = parse_log(args.right, max_settles=args.limit)
        result = diff_settles(
            left.settles,
            right.settles,
            window=args.window,
            tolerance=args.tolerance,
            left_name=args.left,
            right_name=args.right,
        )
        print(json.dumps(result.to_dict()) if args.json else result.render())
        return 0 if result.identical else 1

    parser.print_help()
    return 2


# --------------------------------------------------------------------------
# Self-test
# --------------------------------------------------------------------------

# A synthetic log in exactly the shape the lab jar produces: FRLogger prefix,
# settle lines, then the two per-pass summary lines at the end of each pass.
SYNTHETIC_LOG = """\
2026-08-28 06:20:14.100 INFO   Starting the auto-router
2026-08-28 06:20:24.598 INFO   FRLAB settle 1086 0 0.0 231507.24242124692
2026-08-28 06:20:24.605 INFO   FRLAB settle 42 7 31962.899565443448 231721.83206659596
2026-08-28 06:20:24.607 INFO   FRLAB settle 42 6 48906.619396106544 232697.9860839899
2026-08-28 06:20:25.287 INFO   FRLAB expansions_pass=3 pass=1
2026-08-28 06:20:25.288 INFO   FRLAB expansions_conn n=45 p50=11 p99=1489 max=1489 pass=1
2026-08-28 06:20:25.300 INFO   FRLAB settle 7 0 1.0E7 Infinity
2026-08-28 06:20:25.406 INFO   FRLAB expansions_pass=1 pass=2
2026-08-28 06:20:25.406 INFO   FRLAB expansions_conn n=10 p50=19 p99=432 max=432 pass=2
2026-08-28 06:20:25.556 INFO   Auto-routing stage completed: 0 unrouted
"""

# The same trace as our router would emit it (`FRMETRIC settle`), plus a
# FRMETRIC counters line.
SYNTHETIC_OURS = """\
FRMETRIC settle 1086 0 0.0 231507.24242124692
FRMETRIC settle 42 7 31962.899565443448 231721.83206659596
FRMETRIC settle 42 6 48906.619396106544 232697.9860839899
FRMETRIC settle 7 0 10000000.0 inf
FRMETRIC {"kind":"counters","expansions":4,"tree_queries":11}
"""


class ParseTests(unittest.TestCase):
    def test_pass_records(self):
        log = parse_lines(SYNTHETIC_LOG.splitlines(), source="synthetic")
        self.assertEqual([record.pass_no for record in log.passes], [1, 2])
        self.assertEqual(log.passes[0].expansions, 3)
        self.assertEqual(log.passes[0].conn_n, 45)
        self.assertEqual(log.passes[0].conn_p50, 11)
        self.assertEqual(log.passes[0].conn_p99, 1489)
        self.assertEqual(log.passes[0].conn_max, 1489)
        self.assertEqual(log.passes[1].expansions, 1)
        self.assertEqual(log.total_expansions, 4)

    def test_settle_sequence_and_pass_attribution(self):
        log = parse_lines(SYNTHETIC_LOG.splitlines(), source="synthetic")
        self.assertEqual(len(log.settles), 4)
        self.assertEqual([settle.index for settle in log.settles], [0, 1, 2, 3])
        self.assertEqual(log.settles[0].key, (1086, 0))
        self.assertEqual(log.settles[0].g, 0.0)
        self.assertAlmostEqual(log.settles[0].f, 231507.24242124692)
        self.assertEqual([settle.pass_no for settle in log.settles], [1, 1, 1, 2])
        self.assertEqual([settle.key for settle in log.settles_of_pass(1)],
                         [(1086, 0), (42, 7), (42, 6)])
        self.assertEqual(log.unattributed_settles, 0)

    def test_java_double_spellings(self):
        log = parse_lines(SYNTHETIC_LOG.splitlines())
        self.assertEqual(log.settles[3].g, 1e7)
        self.assertTrue(math.isinf(log.settles[3].f))

    def test_pass_consistency_flag(self):
        log = parse_lines(SYNTHETIC_LOG.splitlines())
        self.assertTrue(all(record.consistent for record in log.passes))
        broken = SYNTHETIC_LOG.replace("expansions_pass=3", "expansions_pass=99")
        self.assertFalse(parse_lines(broken.splitlines()).passes[0].consistent)

    def test_trailing_settles_are_unattributed(self):
        log = parse_lines(SYNTHETIC_LOG.splitlines() + ["INFO FRLAB settle 9 1 1.0 2.0"])
        self.assertEqual(log.settles[-1].pass_no, None)
        self.assertEqual(log.unattributed_settles, 1)

    def test_limit_truncates_settles_only(self):
        log = parse_lines(SYNTHETIC_LOG.splitlines(), max_settles=2)
        self.assertTrue(log.truncated)
        self.assertEqual(len(log.settles), 2)
        self.assertEqual(log.total_expansions, 4)

    def test_frmetric_lines_are_parsed(self):
        log = parse_lines(SYNTHETIC_OURS.splitlines())
        self.assertEqual(len(log.settles), 4)
        self.assertEqual(log.frmetric, [{"kind": "counters", "expansions": 4, "tree_queries": 11}])

    def test_noise_lines_are_ignored(self):
        log = parse_lines(["nothing here", "", "FRLAB something else entirely"])
        self.assertEqual(log.passes, [])
        self.assertEqual(log.settles, [])


class DiffTests(unittest.TestCase):
    def setUp(self):
        self.java = parse_lines(SYNTHETIC_LOG.splitlines()).settles
        self.ours = parse_lines(SYNTHETIC_OURS.splitlines()).settles

    def test_identical_across_tool_prefixes(self):
        result = diff_settles(self.ours, self.java)
        self.assertTrue(result.identical, result.render())
        self.assertIsNone(result.first_divergence)
        self.assertEqual(result.matched_prefix, 4)

    def test_topology_divergence_index_and_context(self):
        drifted = list(self.ours)
        drifted[2] = Settle(index=2, door_id=43, section=6, g=drifted[2].g, f=drifted[2].f)
        result = diff_settles(drifted, self.java, window=1, left_name="ours", right_name="java")
        self.assertEqual(result.kind, TOPOLOGY)
        self.assertEqual(result.first_divergence, 2)
        self.assertEqual(result.matched_prefix, 2)
        self.assertEqual([row[0] for row in result.context], [1, 2, 3])
        self.assertIn("(43, 6)", result.detail)
        self.assertIn("(42, 6)", result.detail)
        self.assertIn(">", result.render())

    def test_length_divergence_when_one_run_stops_early(self):
        result = diff_settles(self.ours[:2], self.java, left_name="ours", right_name="java")
        self.assertEqual(result.kind, LENGTH)
        self.assertEqual(result.first_divergence, 2)
        self.assertEqual(result.left_len, 2)
        self.assertEqual(result.right_len, 4)
        self.assertIn("java", result.detail)

    def test_value_drift_is_reported_below_topology(self):
        drifted = list(self.ours)
        drifted[1] = Settle(index=1, door_id=42, section=7, g=drifted[1].g + 1.0, f=drifted[1].f)
        result = diff_settles(drifted, self.java)
        self.assertEqual(result.kind, VALUE)
        self.assertEqual(result.first_divergence, 1)

    def test_value_drift_within_tolerance_is_identical(self):
        drifted = list(self.ours)
        nudged = drifted[1].g * (1 + 1e-12)
        drifted[1] = Settle(index=1, door_id=42, section=7, g=nudged, f=drifted[1].f)
        self.assertTrue(diff_settles(drifted, self.java, tolerance=1e-9).identical)
        self.assertFalse(diff_settles(drifted, self.java, tolerance=1e-15).identical)

    def test_topology_wins_over_value_drift(self):
        drifted = list(self.ours)
        drifted[1] = Settle(index=1, door_id=42, section=7, g=drifted[1].g + 1.0, f=drifted[1].f)
        drifted[2] = Settle(index=2, door_id=99, section=0, g=drifted[2].g, f=drifted[2].f)
        result = diff_settles(drifted, self.java)
        self.assertEqual(result.kind, TOPOLOGY)
        self.assertEqual(result.first_divergence, 2)

    def test_empty_sequences_are_identical(self):
        self.assertTrue(diff_settles([], []).identical)

    def test_context_window_is_clipped_at_the_ends(self):
        drifted = list(self.ours)
        drifted[0] = Settle(index=0, door_id=1, section=0, g=0.0, f=0.0)
        result = diff_settles(drifted, self.java, window=10)
        self.assertEqual([row[0] for row in result.context], [0, 1, 2, 3])


def _run_self_test() -> int:
    suite = unittest.TestSuite(
        unittest.defaultTestLoader.loadTestsFromTestCase(case)
        for case in (ParseTests, DiffTests)
    )
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    return 0 if result.wasSuccessful() else 1


if __name__ == "__main__":
    sys.exit(main())
