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

# Why the diff does *not* compare door ids by default

M2a established that the two door-id spaces can never coincide. A door id is a
dense counter handed out in the order doors are created, and door creation order
follows the room tiling, which follows `calculate_doors`' neighbour sort. Our
ids are our own; upstream's are upstream's. Even a bit-identical search prints a
different first id, so an id-keyed diff answers `kind=topology first_divergence=0`
on *every* pair of logs and carries no information at all.

The comparison this module performs instead is:

1. **Position-wise on `(g, f)`** — the cost pair at settle *k* of each run, with
   a stated tolerance (`--tolerance`, default `1e-9` relative *and* absolute).
   Costs live in the same board units on both sides, so this is comparable
   across id spaces and it is the quantity the frontier order is derived from.
2. **Aggregates** — expansions per pass, distinct doors and distinct
   `(door, section)` keys touched, and the `g`/`f` distributions. These survive
   any renumbering and are what actually tells you *"we expand 2.1× as many
   elements"* versus *"we expand the same elements in a different order"*.
3. **Topology** — only when a door-id mapping file is supplied with
   `--door-map`; then, and only then, is `(door_id, section)` compared, with our
   ids translated into the reference's space first.

Stdlib only. See `bench/tools/README.md` for the format contract and for how M2
uses this.

Usage:

    python3 bench/tools/frlab_parse.py summary <log> [<log> …] [--json]
    python3 bench/tools/frlab_parse.py diff <ours.log> <java.log> [--window 5]
                                            [--tolerance 1e-9] [--door-map FILE]
    python3 bench/tools/frlab_parse.py stats <ours.log> <java.log> [--json]
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
# Door-id mapping (opt-in topology comparison)
# --------------------------------------------------------------------------


@dataclass(frozen=True)
class DoorMap:
    """Translates our door ids into the reference run's id space.

    Without one, `(door_id, section)` is incomparable between two runs — see the
    module docstring. A mapping file is either

    * **JSON**: `{"doors": {"97": 1086, "129": 42}}`, or the bare object
      `{"97": 1086}`; or
    * **TSV/space-separated text**: one `<ours> <reference>` pair per line,
      `#` comments and blank lines ignored.

    Ids with no entry are left untranslated and counted in `unmapped`, so a
    partial mapping still diffs the part it covers instead of failing.
    """

    doors: dict[int, int] = field(default_factory=dict)
    source: str = "<none>"

    def translate(self, door_id: int) -> int:
        return self.doors.get(door_id, door_id)

    def apply(self, settles: Sequence[Settle]) -> list[Settle]:
        return [replace(settle, door_id=self.translate(settle.door_id)) for settle in settles]

    def unmapped(self, settles: Sequence[Settle]) -> int:
        """How many settles carry a door id the mapping does not cover."""
        return sum(1 for settle in settles if settle.door_id not in self.doors)


def load_door_map(path: str) -> DoorMap:
    with open(path, "r", encoding="utf-8", errors="replace") as handle:
        return parse_door_map(handle.read(), source=path)


def parse_door_map(text: str, source: str = "<memory>") -> DoorMap:
    stripped = text.lstrip()
    if stripped.startswith("{"):
        raw = json.loads(stripped)
        if "doors" in raw and isinstance(raw["doors"], dict):
            raw = raw["doors"]
        return DoorMap({int(key): int(value) for key, value in raw.items()}, source=source)

    doors: dict[int, int] = {}
    for lineno, line in enumerate(text.splitlines(), start=1):
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        fields = line.replace(",", " ").split()
        if len(fields) != 2:
            raise ValueError(f"{source}:{lineno}: expected `<ours> <reference>`, got {line!r}")
        doors[int(fields[0])] = int(fields[1])
    return DoorMap(doors, source=source)


# --------------------------------------------------------------------------
# Aggregates — the renumbering-proof metrics
# --------------------------------------------------------------------------


@dataclass(frozen=True)
class Distribution:
    """Order statistics of one f64 series (`g` or `f` over a settle sequence).

    `nan` and `inf` are counted separately and excluded from the quantiles:
    upstream prints `Infinity` for an unreachable element's sorting value, and
    letting that into the tail would make p99 useless. Quantiles are nearest-rank
    on the sorted finite values — no interpolation, so the numbers are always
    values the run actually produced.
    """

    n: int = 0
    finite: int = 0
    infinite: int = 0
    nan: int = 0
    minimum: float = 0.0
    p50: float = 0.0
    p90: float = 0.0
    p99: float = 0.0
    maximum: float = 0.0
    mean: float = 0.0

    @classmethod
    def of(cls, values: Iterable[float]) -> "Distribution":
        finite: list[float] = []
        infinite = 0
        nan = 0
        total = 0
        for value in values:
            total += 1
            if math.isnan(value):
                nan += 1
            elif math.isinf(value):
                infinite += 1
            else:
                finite.append(value)
        if not finite:
            return cls(n=total, finite=0, infinite=infinite, nan=nan)
        finite.sort()
        return cls(
            n=total,
            finite=len(finite),
            infinite=infinite,
            nan=nan,
            minimum=finite[0],
            p50=_nearest_rank(finite, 0.50),
            p90=_nearest_rank(finite, 0.90),
            p99=_nearest_rank(finite, 0.99),
            maximum=finite[-1],
            mean=sum(finite) / len(finite),
        )

    def to_dict(self) -> dict:
        return {
            "n": self.n,
            "finite": self.finite,
            "infinite": self.infinite,
            "nan": self.nan,
            "min": self.minimum,
            "p50": self.p50,
            "p90": self.p90,
            "p99": self.p99,
            "max": self.maximum,
            "mean": self.mean,
        }


def _nearest_rank(sorted_values: Sequence[float], quantile: float) -> float:
    """Nearest-rank quantile: the smallest value at or above `quantile` of the mass."""
    if not sorted_values:
        return 0.0
    rank = math.ceil(quantile * len(sorted_values))
    return sorted_values[min(max(rank, 1) - 1, len(sorted_values) - 1)]


@dataclass(frozen=True)
class Aggregates:
    """Everything about a run that survives a door-id renumbering."""

    name: str
    settles: int
    expansions_by_pass: dict[int, int]
    settles_by_pass: dict[int, int]
    distinct_doors: int
    distinct_keys: int
    g: Distribution
    f: Distribution

    @property
    def total_expansions(self) -> int | None:
        """`None`, not `0`, when the log carries no `expansions_pass` line.

        Our `--settle-trace` file holds settle records only, so reporting `0`
        there would read as "we expanded nothing" next to the jar's real total.
        """
        if not self.expansions_by_pass:
            return None
        return sum(self.expansions_by_pass.values())

    @classmethod
    def of(cls, log: LabLog, name: str | None = None) -> "Aggregates":
        settles_by_pass: dict[int, int] = {}
        for settle in log.settles:
            if settle.pass_no is not None:
                settles_by_pass[settle.pass_no] = settles_by_pass.get(settle.pass_no, 0) + 1
        return cls(
            name=name or log.source,
            settles=len(log.settles),
            expansions_by_pass={
                record.pass_no: record.expansions
                for record in log.passes
                if record.expansions is not None
            },
            settles_by_pass=settles_by_pass,
            distinct_doors=len({settle.door_id for settle in log.settles}),
            distinct_keys=len({settle.key for settle in log.settles}),
            g=Distribution.of(settle.g for settle in log.settles),
            f=Distribution.of(settle.f for settle in log.settles),
        )

    def to_dict(self) -> dict:
        return {
            "name": self.name,
            "settles": self.settles,
            "total_expansions": self.total_expansions,
            "expansions_by_pass": {str(k): v for k, v in sorted(self.expansions_by_pass.items())},
            "settles_by_pass": {str(k): v for k, v in sorted(self.settles_by_pass.items())},
            "distinct_doors": self.distinct_doors,
            "distinct_keys": self.distinct_keys,
            "g": self.g.to_dict(),
            "f": self.f.to_dict(),
        }


@dataclass(frozen=True)
class AggregateDiff:
    """Two `Aggregates` side by side, with the ratios that matter for M2/M3.

    `expansion_ratio` is the headline: > 1 means we settle more elements than
    the reference, which is a tiling/cost divergence and not a constant-factor
    performance problem. A ratio of 1 with a slower wall time is the opposite
    finding — the same search, executed slower.
    """

    left: Aggregates
    right: Aggregates

    @property
    def expansion_ratio(self) -> float | None:
        return _ratio(self.left.settles, self.right.settles)

    @property
    def distinct_door_ratio(self) -> float | None:
        return _ratio(self.left.distinct_doors, self.right.distinct_doors)

    def pass_rows(self) -> list[tuple[int, int | None, int | None, float | None]]:
        """`(pass, left expansions, right expansions, ratio)` over the union of passes."""
        passes = sorted(set(self.left.expansions_by_pass) | set(self.right.expansions_by_pass))
        rows = []
        for pass_no in passes:
            left = self.left.expansions_by_pass.get(pass_no)
            right = self.right.expansions_by_pass.get(pass_no)
            rows.append((pass_no, left, right, _ratio(left, right)))
        return rows

    def to_dict(self) -> dict:
        return {
            "left": self.left.to_dict(),
            "right": self.right.to_dict(),
            "expansion_ratio": self.expansion_ratio,
            "distinct_door_ratio": self.distinct_door_ratio,
            "passes": [
                {"pass": p, "left": l, "right": r, "ratio": ratio}
                for p, l, r, ratio in self.pass_rows()
            ],
        }

    def render(self) -> str:
        lines = [
            f"{'metric':<22} {self.left.name[-34:]:>34} {self.right.name[-34:]:>34} {'ratio':>9}",
            _agg_row("settles", self.left.settles, self.right.settles),
            _agg_row("expansions (all passes)", self.left.total_expansions, self.right.total_expansions),
            _agg_row("distinct doors", self.left.distinct_doors, self.right.distinct_doors),
            _agg_row("distinct (door,sec)", self.left.distinct_keys, self.right.distinct_keys),
        ]
        rows = self.pass_rows()
        if rows:
            lines.append("")
            lines.append(f"{'pass':<22} {'left':>34} {'right':>34} {'ratio':>9}")
            for pass_no, left, right, ratio in rows:
                lines.append(_agg_row(f"  expansions pass {pass_no}", left, right, ratio))
        for label, left_dist, right_dist in (("g", self.left.g, self.right.g), ("f", self.left.f, self.right.f)):
            lines.append("")
            lines.append(f"{label + ' distribution':<22} {'left':>34} {'right':>34} {'ratio':>9}")
            for stat in ("min", "p50", "p90", "p99", "max", "mean"):
                lines.append(
                    _agg_row(f"  {label} {stat}", left_dist.to_dict()[stat], right_dist.to_dict()[stat])
                )
            lines.append(
                _agg_row(f"  {label} non-finite", left_dist.infinite + left_dist.nan,
                         right_dist.infinite + right_dist.nan)
            )
        return "\n".join(lines)


def _ratio(left: float | None, right: float | None) -> float | None:
    if left is None or right is None or right == 0:
        return None
    if math.isnan(left) or math.isnan(right) or math.isinf(right):
        return None
    return left / right


def _agg_row(label: str, left, right, ratio: float | None = None) -> str:
    if ratio is None:
        ratio = _ratio(left, right)
    ratio_text = "-" if ratio is None else f"{ratio:.3f}"
    return f"{label:<22} {_fmt(left):>34} {_fmt(right):>34} {ratio_text:>9}"


def _fmt(value) -> str:
    if value is None:
        return "-"
    if isinstance(value, float):
        return f"{value:.6g}"
    return str(value)


def diff_aggregates(left: LabLog, right: LabLog, *, left_name: str | None = None,
                    right_name: str | None = None) -> AggregateDiff:
    return AggregateDiff(Aggregates.of(left, left_name), Aggregates.of(right, right_name))


# --------------------------------------------------------------------------
# Differential harness
# --------------------------------------------------------------------------

IDENTICAL = "identical"
TOPOLOGY = "topology"
LENGTH = "length"
VALUE = "value"


@dataclass
class SettleDiff:
    """The result of comparing two settle sequences **by sequence position**.

    Position *k* of one run is compared with position *k* of the other on the
    cost pair `(g, f)` under `tolerance`; `(door_id, section)` is compared only
    when a `DoorMap` was supplied, because the two id spaces are independent
    counters (see the module docstring).

    `kind` is one of:

    - `identical` — same length and, within tolerance, the same `(g, f)` at
                    every position (plus the same keys, if a map was given)
    - `topology`  — **opt-in only.** With a `DoorMap`, the two runs settled a
                    different `(door_id, section)` at `first_divergence`
    - `length`    — `(g, f)` agrees over the whole shorter sequence but one run
                    stopped early (a stop rule fired, or a trace was truncated)
    - `value`     — `g` or `f` differs beyond the tolerance at
                    `first_divergence`: from here on the two frontiers hold
                    different costs, so the orders cannot be expected to agree

    `tolerance` is recorded on the result so a report always states the
    threshold its verdict was reached under.
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
    tolerance: float = 1e-9
    door_map: str | None = None
    aggregates: AggregateDiff | None = None

    @property
    def identical(self) -> bool:
        return self.kind == IDENTICAL

    @property
    def compared_topology(self) -> bool:
        return self.door_map is not None

    def to_dict(self) -> dict:
        payload = {
            "kind": self.kind,
            "first_divergence": self.first_divergence,
            "matched_prefix": self.matched_prefix,
            "left": {"name": self.left_name, "settles": self.left_len},
            "right": {"name": self.right_name, "settles": self.right_len},
            "detail": self.detail,
            "tolerance": self.tolerance,
            "compared": "g/f by position" + (" + topology" if self.compared_topology else ""),
            "door_map": self.door_map,
        }
        if self.aggregates is not None:
            payload["aggregates"] = self.aggregates.to_dict()
        return payload

    def render(self) -> str:
        """A human-readable divergence report with the context window."""
        compared = "g/f by sequence position"
        if self.compared_topology:
            compared += f" + topology (door map {self.door_map})"
        lines = [
            f"{self.left_name}: {self.left_len} settles",
            f"{self.right_name}: {self.right_len} settles",
            f"compared: {compared}, tolerance={self.tolerance:g} (rel and abs)",
            f"kind={self.kind} matched_prefix={self.matched_prefix} "
            f"first_divergence={self.first_divergence}",
        ]
        if self.detail:
            lines.append(self.detail)
        if self.aggregates is not None:
            lines.append("")
            lines.append(self.aggregates.render())
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
    """`tolerance` is used as both the relative and the absolute bound.

    The absolute half matters: the first settle of a search always has `g = 0.0`,
    and a relative-only test would reject any non-zero counterpart however small.
    `NaN` matches `NaN` and `±inf` matches itself (`math.isclose` handles the
    latter).
    """
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
    door_map: DoorMap | None = None,
    aggregates: AggregateDiff | None = None,
) -> SettleDiff:
    """Finds the first sequence position at which two settle sequences diverge.

    The comparison is positional: settle *k* of `left` against settle *k* of
    `right`, on `(g, f)` under `tolerance` (used as both the relative and the
    absolute bound, so a cost of `0.0` on one side and `1e-12` on the other
    still matches). `NaN` matches `NaN`; `+inf` matches `+inf`.

    `(door_id, section)` is compared **only** when `door_map` is given, and then
    with `left`'s ids translated into `right`'s space first. Without a map the
    ids are two independent counters and comparing them is meaningless — every
    pair of logs would answer `topology` at index 0.

    Severity order, when a map *is* supplied: topology, then value, then length.
    Without a map: value, then length.
    """
    if door_map is not None:
        left = door_map.apply(left)

    shorter = min(len(left), len(right))

    if door_map is not None:
        topology_at: int | None = None
        for index in range(shorter):
            if left[index].key != right[index].key:
                topology_at = index
                break

        if topology_at is not None:
            detail = (
                f"{left_name} settled {left[topology_at].key} (mapped), "
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
                tolerance=tolerance,
                door_map=door_map.source,
                aggregates=aggregates,
            )

    value_at: int | None = None
    for index in range(shorter):
        if not _close(left[index].g, right[index].g, tolerance) or not _close(
            left[index].f, right[index].f, tolerance
        ):
            value_at = index
            break

    if len(left) != len(right) and (value_at is None or value_at >= shorter):
        # `(g, f)` agrees everywhere both sequences have a settle; the shorter
        # run stopped first.
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
            detail=f"g/f agree for {shorter} settles, then only {longer_name} continues",
            tolerance=tolerance,
            door_map=door_map.source if door_map else None,
            aggregates=aggregates,
        )

    if value_at is not None:
        detail = (
            f"g/f drift at position {value_at}: "
            f"{left_name} g={left[value_at].g!r} f={left[value_at].f!r}, "
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
            tolerance=tolerance,
            door_map=door_map.source if door_map else None,
            aggregates=aggregates,
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
        tolerance=tolerance,
        door_map=door_map.source if door_map else None,
        aggregates=aggregates,
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

    diff = subparsers.add_parser(
        "diff",
        help="diff two settle sequences by position on (g, f), plus aggregates",
    )
    diff.add_argument("left", help="our log")
    diff.add_argument("right", help="the lab jar's log (the reference)")
    diff.add_argument("--window", type=int, default=5, help="context records either side")
    diff.add_argument(
        "--tolerance",
        type=float,
        default=1e-9,
        help="g/f tolerance, used as both the relative and the absolute bound",
    )
    diff.add_argument("--limit", type=int, default=None, help="compare at most N settles")
    diff.add_argument(
        "--door-map",
        default=None,
        metavar="FILE",
        help="opt in to the (door_id, section) comparison, translating our ids "
        "through FILE (JSON object or `<ours> <reference>` lines)",
    )
    diff.add_argument(
        "--no-aggregates", action="store_true", help="omit the aggregate metrics block"
    )
    diff.add_argument("--json", action="store_true", help="emit JSON instead of a report")

    stats = subparsers.add_parser(
        "stats", help="aggregate metrics of two logs side by side (no positional diff)"
    )
    stats.add_argument("left")
    stats.add_argument("right")
    stats.add_argument("--limit", type=int, default=None, help="collect at most N settles")
    stats.add_argument("--json", action="store_true", help="emit JSON instead of a table")

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
        door_map = load_door_map(args.door_map) if args.door_map else None
        result = diff_settles(
            left.settles,
            right.settles,
            window=args.window,
            tolerance=args.tolerance,
            left_name=args.left,
            right_name=args.right,
            door_map=door_map,
            aggregates=None if args.no_aggregates else diff_aggregates(left, right),
        )
        print(json.dumps(result.to_dict()) if args.json else result.render())
        if door_map is not None:
            unmapped = door_map.unmapped(left.settles)
            if unmapped:
                print(
                    f"warning: {unmapped}/{len(left.settles)} settles carry a door id "
                    f"absent from {door_map.source}; those were left untranslated",
                    file=sys.stderr,
                )
        return 0 if result.identical else 1

    if args.command == "stats":
        comparison = diff_aggregates(
            parse_log(args.left, max_settles=args.limit),
            parse_log(args.right, max_settles=args.limit),
        )
        print(json.dumps(comparison.to_dict()) if args.json else comparison.render())
        return 0

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

    def test_renumbered_doors_alone_are_still_identical(self):
        """The M2a finding: an id-keyed diff called every pair `topology` at 0.

        Same costs, same order, every door id shifted by 1000 — the two id
        spaces are independent counters, so this must read `identical`.
        """
        renumbered = [replace(settle, door_id=settle.door_id + 1000) for settle in self.ours]
        result = diff_settles(renumbered, self.java, left_name="ours", right_name="java")
        self.assertTrue(result.identical, result.render())
        self.assertFalse(result.compared_topology)

    def test_length_divergence_when_one_run_stops_early(self):
        result = diff_settles(self.ours[:2], self.java, left_name="ours", right_name="java")
        self.assertEqual(result.kind, LENGTH)
        self.assertEqual(result.first_divergence, 2)
        self.assertEqual(result.left_len, 2)
        self.assertEqual(result.right_len, 4)
        self.assertIn("java", result.detail)

    def test_value_drift_at_a_position(self):
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

    def test_zero_costs_need_the_absolute_bound(self):
        # g = 0.0 on one side: a relative-only test would reject any non-zero
        # counterpart however small.
        drifted = list(self.ours)
        drifted[0] = Settle(index=0, door_id=1086, section=0, g=1e-12, f=drifted[0].f)
        self.assertTrue(diff_settles(drifted, self.java, tolerance=1e-9).identical)

    def test_infinities_and_nans_match_themselves(self):
        self.assertTrue(_close(math.inf, math.inf, 1e-9))
        self.assertTrue(_close(math.nan, math.nan, 1e-9))
        self.assertFalse(_close(math.inf, 1e300, 1e-9))
        self.assertFalse(_close(math.nan, 0.0, 1e-9))

    def test_tolerance_is_recorded_on_the_result(self):
        result = diff_settles(self.ours, self.java, tolerance=1e-6)
        self.assertEqual(result.tolerance, 1e-6)
        self.assertIn("tolerance=1e-06", result.render())

    def test_empty_sequences_are_identical(self):
        self.assertTrue(diff_settles([], []).identical)

    def test_context_window_is_clipped_at_the_ends(self):
        drifted = list(self.ours)
        drifted[0] = Settle(index=0, door_id=1086, section=0, g=99.0, f=0.0)
        result = diff_settles(drifted, self.java, window=10)
        self.assertEqual([row[0] for row in result.context], [0, 1, 2, 3])


class DoorMapTests(unittest.TestCase):
    def setUp(self):
        self.java = parse_lines(SYNTHETIC_LOG.splitlines()).settles
        self.ours = parse_lines(SYNTHETIC_OURS.splitlines()).settles

    def test_json_and_text_forms_agree(self):
        as_json = parse_door_map('{"doors": {"1": 1086, "2": 42}}')
        as_text = parse_door_map("# ours reference\n1 1086\n2\t42\n\n")
        self.assertEqual(as_json.doors, as_text.doors)
        self.assertEqual(as_json.translate(1), 1086)
        self.assertEqual(as_json.translate(999), 999)

    def test_bare_json_object_is_accepted(self):
        self.assertEqual(parse_door_map('{"7": 8}').doors, {7: 8})

    def test_malformed_text_line_is_rejected(self):
        with self.assertRaises(ValueError):
            parse_door_map("1 2 3\n")

    def test_topology_is_only_compared_with_a_map(self):
        shifted = [replace(settle, door_id=settle.door_id + 1000) for settle in self.ours]
        # No map: identical.
        self.assertTrue(diff_settles(shifted, self.java).identical)
        # A map that undoes the shift: still identical, but topology was checked.
        good = DoorMap({settle.door_id + 1000: settle.door_id for settle in self.ours}, "good")
        result = diff_settles(shifted, self.java, door_map=good)
        self.assertTrue(result.identical, result.render())
        self.assertTrue(result.compared_topology)
        self.assertIn("door map good", result.render())

    def test_map_that_disagrees_reports_topology(self):
        shifted = [replace(settle, door_id=settle.door_id + 1000) for settle in self.ours]
        wrong = DoorMap({settle.door_id + 1000: settle.door_id for settle in self.ours}, "wrong")
        # Door 7 is the last settle's, and unique to it, so exactly position 3
        # is mistranslated.
        wrong.doors[1007] = 4242
        result = diff_settles(shifted, self.java, door_map=wrong, left_name="ours", right_name="java")
        self.assertEqual(result.kind, TOPOLOGY)
        self.assertEqual(result.first_divergence, 3)
        self.assertIn("(4242, 0)", result.detail)

    def test_unmapped_ids_are_counted_not_fatal(self):
        partial = DoorMap({1086: 1086}, "partial")
        self.assertEqual(partial.unmapped(self.ours), 3)
        self.assertEqual(partial.apply(self.ours)[1].door_id, self.ours[1].door_id)


class AggregateTests(unittest.TestCase):
    def setUp(self):
        self.java_log = parse_lines(SYNTHETIC_LOG.splitlines(), source="java")
        self.ours_log = parse_lines(SYNTHETIC_OURS.splitlines(), source="ours")

    def test_distribution_excludes_non_finite_from_quantiles(self):
        dist = Distribution.of([1.0, 2.0, 3.0, 4.0, math.inf, math.nan])
        self.assertEqual((dist.n, dist.finite, dist.infinite, dist.nan), (6, 4, 1, 1))
        self.assertEqual(dist.minimum, 1.0)
        self.assertEqual(dist.maximum, 4.0)
        self.assertEqual(dist.p50, 2.0)  # nearest rank: ceil(0.5 * 4) = 2
        self.assertEqual(dist.p99, 4.0)
        self.assertEqual(dist.mean, 2.5)

    def test_empty_distribution_is_all_zero(self):
        dist = Distribution.of([])
        self.assertEqual(dist.n, 0)
        self.assertEqual(dist.finite, 0)
        self.assertEqual(dist.p50, 0.0)

    def test_no_pass_lines_reports_none_not_zero(self):
        # A `--settle-trace` file holds settle records only.
        agg = Aggregates.of(self.ours_log)
        self.assertEqual(agg.settles, 4)
        self.assertIsNone(agg.total_expansions)
        self.assertEqual(agg.expansions_by_pass, {})
        self.assertIn("-", _agg_row("expansions", agg.total_expansions, 4))

    def test_aggregates_of_a_log(self):
        agg = Aggregates.of(self.java_log)
        self.assertEqual(agg.settles, 4)
        self.assertEqual(agg.total_expansions, 4)
        self.assertEqual(agg.expansions_by_pass, {1: 3, 2: 1})
        self.assertEqual(agg.settles_by_pass, {1: 3, 2: 1})
        self.assertEqual(agg.distinct_doors, 3)  # 1086, 42, 7
        self.assertEqual(agg.distinct_keys, 4)  # (42, 7) and (42, 6) are distinct
        self.assertEqual(agg.f.infinite, 1)

    def test_aggregate_ratios_and_pass_rows(self):
        doubled = parse_lines((SYNTHETIC_LOG + SYNTHETIC_LOG).splitlines(), source="doubled")
        comparison = diff_aggregates(doubled, self.java_log)
        self.assertAlmostEqual(comparison.expansion_ratio, 2.0)
        rows = comparison.pass_rows()
        self.assertEqual([row[0] for row in rows], [1, 2])
        # The second copy overwrites the pass records rather than summing them,
        # so per-pass expansions stay equal while the settle count doubles.
        self.assertAlmostEqual(rows[0][3], 1.0)

    def test_ratio_guards_against_zero_and_non_finite(self):
        self.assertIsNone(_ratio(1, 0))
        self.assertIsNone(_ratio(None, 3))
        self.assertIsNone(_ratio(1.0, math.inf))
        self.assertIsNone(_ratio(math.nan, 1.0))

    def test_aggregates_appear_in_the_diff_report(self):
        result = diff_settles(
            self.ours_log.settles,
            self.java_log.settles,
            aggregates=diff_aggregates(self.ours_log, self.java_log),
        )
        rendered = result.render()
        self.assertIn("distinct doors", rendered)
        self.assertIn("g distribution", rendered)
        self.assertIn("aggregates", result.to_dict())


def _run_self_test() -> int:
    suite = unittest.TestSuite(
        unittest.defaultTestLoader.loadTestsFromTestCase(case)
        for case in (ParseTests, DiffTests, DoorMapTests, AggregateTests)
    )
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    return 0 if result.wasSuccessful() else 1


if __name__ == "__main__":
    sys.exit(main())
