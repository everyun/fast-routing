#!/usr/bin/env python3
"""G1.2a checker parity: our clearance violations vs `java -drc` on the input board.

`docs/ROADMAP.md` G1.2a:

> fr-check's violation multiset `(pos_a, pos_b, expected_clearance)` on every
> **input board** equals the `java -drc` JSON (types clearance+holeClearance).

The oracles are `bench/golden/drc/<fixture>.json`, produced by the released
2.3.0 jar. Our side is produced by `cargo run -p fr-check --example drc_report`,
which emits the same `drc.v1.json` shape (see that file's module docs). Nothing
here is routed: both sides describe the board as imported, so a disagreement is
an importer bug, a clearance-matrix bug or a checker bug and never a router one.

## The key, and why it is what it is

A violation record carries no item identity we can compare — `uuid` is the
decimal item id, and item ids are assignment-order artefacts of each importer.
What it does carry is, per item, the **bounding-box centre of gravity** in the
report unit, and, inside `description`, the required clearance formatted with
`%.4f`. So the key is

    ( min(pos_a, pos_b), max(pos_a, pos_b), expected )

with the pair sorted because which item upstream calls `first_item` depends on
the order its `board.get_items()` scan happened to reach them in
(`DesignRulesChecker.getAllClearanceViolations`), and *that* order is not part
of the semantics.

Multiset, not set: upstream deduplicates by `(min id, max id, layer)`, so one
pair overlapping on three layers is three records with identical positions. The
multiplicity is signal — it says how many layers are involved — and dropping it
would let a checker that finds a pair on one layer instead of three still pass.

## Tolerances

Positions default to **exact** float equality. They are honest rationals on both
sides: an integer board coordinate divided by the integer scale factor, then
optionally multiplied by one unit ratio and divided by another, in IEEE doubles
performing the same operations in the same order. If a fixture needs
`--pos-decimals` to pass, that is a finding to write down, not a knob to turn —
the flag exists so the *size* of a disagreement can be measured.

`expected` is compared as its 4-decimal rendering, because that is all Java's
report preserves (it lives only inside the `description` string). Our side
publishes the raw double in `expected_mm` and this tool does the rounding
itself, in Python, so the comparison never depends on Rust's round-half-even and
Java's `%.4f` round-half-up agreeing on a tie.

## Usage

    python3 bench/tools/drc_compare.py --fixture Issue190-processor.Z80 \\
        [--ours-dir DIR] [--explain 5]
    python3 bench/tools/drc_compare.py --all [--ours-dir DIR] [--json]
    python3 bench/tools/drc_compare.py --all --bridge-arg --pin-frame-fix \\
        --ours-dir bench/results/drc-ours-pinfix
    python3 bench/tools/drc_compare.py --self-test

The tool builds and runs the Rust bridge itself, caching each report in
`--ours-dir` (default `bench/results/drc-ours`); `--no-build` reuses that cache.
`--bridge-arg` forwards a flag to the bridge, which is how the `--pin-frame-fix`
experiment (see the bridge's module docs) is measured against the same gate.

Exit codes: 0 = every compared fixture is an exact multiset match, 1 = at least
one mismatch, 2 = usage / IO error.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from collections import Counter
from decimal import Decimal, ROUND_HALF_UP
from pathlib import Path

# The two types G1.2a compares. Upstream's report also carries `track_dangling`
# and `via_dangling`, which come from `getAllUnconnectedItems` and belong to the
# ratsnest (G1.2b), not to the clearance checker.
COMPARED_TYPES = ("clearance", "hole_clearance")

REPO = Path(__file__).resolve().parents[2]
GOLDEN_DIR = REPO / "bench" / "golden" / "drc"
DEFAULT_OURS_DIR = REPO / "bench" / "results" / "drc-ours"
DEFAULT_FIXTURES_DIR = Path(
    os.environ.get("FR_FIXTURES_DIR", "/tmp/freerouting-upstream/fixtures")
)
BRIDGE = REPO / "target" / "release" / "examples" / "drc_report"

# "... (expected: 0.1906 mm, actual: 0.0000 mm)" — the only place Java records
# the required clearance.
EXPECTED_RE = re.compile(r"\(expected:\s*(-?[0-9.]+)\s*(\S+),\s*actual:\s*(-?[0-9.]+)")

# "close" in the summary table: counts within 1 % of the oracle's.
CLOSE_FRACTION = 0.01


# ---------------------------------------------------------------------------
# loading
# ---------------------------------------------------------------------------


def quantize(value: float, decimals: int = 4) -> str:
    """`%.4f` on a double, done in decimal with round-half-up like Java's."""
    step = Decimal(1).scaleb(-decimals)
    return str(Decimal(repr(float(value))).quantize(step, rounding=ROUND_HALF_UP))


def _position(item: dict) -> tuple[float, float]:
    pos = item.get("pos") or {}
    return (float(pos.get("x", 0.0)), float(pos.get("y", 0.0)))


def parse_expected(record: dict) -> str | None:
    """The required clearance, as its 4-decimal rendering.

    Prefers our explicit `expected_mm`; falls back to the `description` string,
    which is the only carrier on the Java side.
    """
    if "expected_mm" in record:
        return quantize(record["expected_mm"])
    match = EXPECTED_RE.search(record.get("description", ""))
    if match is None:
        return None
    # Java already printed 4 decimals; re-quantize so both sides are one shape.
    return quantize(float(match.group(1)))


def load_records(path: Path) -> list[dict]:
    """Every clearance/hole-clearance record of one report, normalised.

    Each record is `{type, pos_a, pos_b, expected, description}` with the two
    positions sorted, so the caller never has to think about which item upstream
    called first.
    """
    with path.open(encoding="utf-8") as handle:
        document = json.load(handle)
    out = []
    for raw in document.get("violations", []):
        kind = raw.get("type")
        if kind not in COMPARED_TYPES:
            continue
        items = raw.get("items", [])
        if len(items) != 2:
            raise ValueError(f"{path}: {kind} record with {len(items)} items")
        expected = parse_expected(raw)
        if expected is None:
            raise ValueError(f"{path}: no expected clearance in {raw.get('description')!r}")
        first, second = sorted(_position(item) for item in items)
        out.append(
            {
                "type": kind,
                "pos_a": first,
                "pos_b": second,
                "expected": expected,
                "description": raw.get("description", ""),
            }
        )
    return out


def key_of(record: dict, pos_decimals: int | None) -> tuple:
    """The G1.2a match key. `pos_decimals=None` keeps positions exact."""
    if pos_decimals is None:
        pos_a, pos_b = record["pos_a"], record["pos_b"]
    else:
        pos_a = tuple(round(v, pos_decimals) for v in record["pos_a"])
        pos_b = tuple(round(v, pos_decimals) for v in record["pos_b"])
    return (record["type"], pos_a, pos_b, record["expected"])


# ---------------------------------------------------------------------------
# comparison
# ---------------------------------------------------------------------------


def compare(java_records: list[dict], our_records: list[dict], pos_decimals: int | None) -> dict:
    java_keys = Counter(key_of(r, pos_decimals) for r in java_records)
    our_keys = Counter(key_of(r, pos_decimals) for r in our_records)
    missing = java_keys - our_keys  # in the oracle, not in ours
    extra = our_keys - java_keys  # in ours, not in the oracle
    matched = sum((java_keys & our_keys).values())

    by_type = {}
    for kind in COMPARED_TYPES:
        by_type[kind] = {
            "java": sum(1 for r in java_records if r["type"] == kind),
            "ours": sum(1 for r in our_records if r["type"] == kind),
        }

    return {
        "java_count": len(java_records),
        "our_count": len(our_records),
        "matched": matched,
        "missing": sum(missing.values()),
        "extra": sum(extra.values()),
        "by_type": by_type,
        "pass": not missing and not extra,
        "missing_keys": missing,
        "extra_keys": extra,
    }


def classify(result: dict) -> str:
    """`exact` / `close` (count within 1 %) / `far`, per the gate report format."""
    if result["pass"]:
        return "exact"
    java, ours = result["java_count"], result["our_count"]
    if java == 0:
        return "far" if ours else "exact"
    if abs(ours - java) <= max(1, CLOSE_FRACTION * java):
        return "close"
    return "far"


# ---------------------------------------------------------------------------
# running the bridge
# ---------------------------------------------------------------------------


def fixture_names() -> list[str]:
    return sorted(p.stem for p in GOLDEN_DIR.glob("*.json"))


def dsn_for(fixture: str, fixtures_dir: Path) -> Path:
    return fixtures_dir / f"{fixture}.dsn"


def build_bridge() -> None:
    subprocess.run(
        ["cargo", "build", "-p", "fr-check", "--example", "drc_report", "--release"],
        cwd=REPO,
        check=True,
    )


def run_bridge(
    fixture: str,
    fixtures_dir: Path,
    ours_dir: Path,
    bridge_args: list[str] | None = None,
) -> tuple[Path | None, str, str]:
    """Produces our report for one fixture. Returns `(path, status, note)`.

    `status` is `""` on success, `"unimportable"` when the bridge refused the
    board and `"no-fixture"` when the `.dsn` is not on disk at all. The two are
    kept apart on purpose: a missing fixture is a broken harness (run
    `bench/fetch_fixtures.sh`) and must never be reported as a board we cannot
    read.
    """
    dsn = dsn_for(fixture, fixtures_dir)
    if not dsn.exists():
        return None, "no-fixture", f"no .dsn at {dsn} — run bench/fetch_fixtures.sh"
    ours_dir.mkdir(parents=True, exist_ok=True)
    out = ours_dir / f"{fixture}.json"
    try:
        proc = subprocess.run(
            [str(BRIDGE), *(bridge_args or []), str(dsn), str(out)],
            capture_output=True,
            text=True,
            timeout=600,
        )
    except FileNotFoundError:
        return None, "unimportable", f"bridge not built: {BRIDGE}"
    except subprocess.TimeoutExpired:
        return None, "unimportable", "bridge timed out after 600 s"
    if proc.returncode != 0:
        message = (proc.stderr or proc.stdout or "").strip().splitlines()
        return None, "unimportable", message[-1] if message else f"exit {proc.returncode}"
    return out, "", ""


# ---------------------------------------------------------------------------
# reporting
# ---------------------------------------------------------------------------


def explain(result: dict, java_records: list[dict], our_records: list[dict], limit: int) -> list[str]:
    """The first `limit` unmatched records of each side, as readable lines."""
    lines = []
    java_by_key = {}
    for record in java_records:
        java_by_key.setdefault(key_of(record, None), record)
    our_by_key = {}
    for record in our_records:
        our_by_key.setdefault(key_of(record, None), record)

    for label, keys, source in (
        ("java-only", result["missing_keys"], java_by_key),
        ("ours-only", result["extra_keys"], our_by_key),
    ):
        shown = 0
        for key, count in keys.most_common():
            if shown >= limit:
                lines.append(f"    {label}: … {len(keys) - shown} more distinct keys")
                break
            record = source.get(key)
            pos_a, pos_b = key[1], key[2]
            lines.append(
                f"    {label} x{count} {key[0]:<15} expected={key[3]} "
                f"a={pos_a[0]:.4f},{pos_a[1]:.4f} b={pos_b[0]:.4f},{pos_b[1]:.4f}"
            )
            if record is not None:
                lines.append(f"        {record['description']}")
            shown += 1
    return lines


def one_fixture(
    fixture: str,
    fixtures_dir: Path,
    ours_dir: Path,
    pos_decimals: int | None,
    build: bool,
    bridge_args: list[str] | None = None,
) -> dict:
    golden = GOLDEN_DIR / f"{fixture}.json"
    if not golden.exists():
        return {"fixture": fixture, "status": "no-oracle", "note": str(golden)}
    java_records = load_records(golden)

    ours_path = ours_dir / f"{fixture}.json"
    note = ""
    if build or not ours_path.exists():
        ours_path, failure, note = run_bridge(fixture, fixtures_dir, ours_dir, bridge_args)
        if ours_path is None:
            return {
                "fixture": fixture,
                "status": failure,
                "note": note,
                "java_count": len(java_records),
                "our_count": 0,
            }
    our_records = load_records(ours_path)
    result = compare(java_records, our_records, pos_decimals)
    result.update(
        {
            "fixture": fixture,
            "status": classify(result),
            "note": note,
            "_java_records": java_records,
            "_our_records": our_records,
        }
    )
    return result


def summary_table(results: list[dict]) -> str:
    header = (
        f"{'fixture':<44} {'java':>6} {'ours':>6} {'match':>6} {'miss':>6} "
        f"{'extra':>6}  {'jcl/jhole':>11} {'ocl/ohole':>11}  status"
    )
    lines = [header, "-" * len(header)]
    for result in results:
        if result["status"] in ("unimportable", "no-oracle", "no-fixture"):
            lines.append(
                f"{result['fixture']:<44} {result.get('java_count', 0):>6} {'-':>6} "
                f"{'-':>6} {'-':>6} {'-':>6}  {'-':>11} {'-':>11}  "
                f"{result['status']}: {result.get('note', '')}"
            )
            continue
        by = result["by_type"]
        jsplit = f"{by['clearance']['java']}/{by['hole_clearance']['java']}"
        osplit = f"{by['clearance']['ours']}/{by['hole_clearance']['ours']}"
        lines.append(
            f"{result['fixture']:<44} {result['java_count']:>6} {result['our_count']:>6} "
            f"{result['matched']:>6} {result['missing']:>6} {result['extra']:>6}  "
            f"{jsplit:>11} {osplit:>11}  {result['status']}"
        )
    counts = Counter(r["status"] for r in results)
    lines.append("")
    lines.append(
        "totals: "
        + ", ".join(
            f"{name}={counts.get(name, 0)}"
            for name in ("exact", "close", "far", "unimportable", "no-fixture", "no-oracle")
        )
        + f"  ({len(results)} fixtures)"
    )
    graded = [r for r in results if r["status"] in ("exact", "close", "far")]
    lines.append(
        f"records: java={sum(r['java_count'] for r in graded)} "
        f"ours={sum(r['our_count'] for r in graded)} "
        f"matched={sum(r['matched'] for r in graded)}"
    )
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# self-test
# ---------------------------------------------------------------------------


def _record(kind, ax, ay, bx, by, expected_mm):
    return {
        "type": kind,
        "severity": "error",
        "description": f"x (expected: {expected_mm:.4f} mm, actual: 0.0000 mm)",
        "items": [
            {"description": "Pin", "pos": {"x": ax, "y": ay}, "uuid": "1"},
            {"description": "Pin", "pos": {"x": bx, "y": by}, "uuid": "2"},
        ],
    }


def self_test() -> int:
    failures = []
    ran = 0

    def check(name, condition):
        nonlocal ran
        ran += 1
        if not condition:
            failures.append(name)

    check("quantize rounds half up", quantize(0.19065, 4) == "0.1907")
    check("quantize pads", quantize(0.5, 4) == "0.5000")

    java = [
        _record("hole_clearance", 1.0, 2.0, 3.0, 4.0, 0.2),
        _record("hole_clearance", 3.0, 4.0, 1.0, 2.0, 0.2),  # same pair, other order
        _record("clearance", 9.0, 9.0, 8.0, 8.0, 0.5),
        _record("track_dangling", 0.0, 0.0, 0.0, 0.0, 0.0),
    ]
    ours = [
        _record("hole_clearance", 3.0, 4.0, 1.0, 2.0, 0.2),
        _record("hole_clearance", 1.0, 2.0, 3.0, 4.0, 0.2),
        _record("clearance", 8.0, 8.0, 9.0, 9.0, 0.5),
    ]
    tmp = Path(os.environ.get("TMPDIR", "/tmp"))
    java_path = tmp / "drc_compare_selftest_java.json"
    our_path = tmp / "drc_compare_selftest_ours.json"
    java_path.write_text(json.dumps({"violations": java}), encoding="utf-8")
    our_path.write_text(json.dumps({"violations": ours}), encoding="utf-8")

    jr = load_records(java_path)
    orr = load_records(our_path)
    check("dangling types are filtered out", len(jr) == 3)
    check("item order is normalised", compare(jr, orr, None)["pass"])
    check("multiplicity is kept", compare(jr, orr[:-1], None)["missing"] == 1)

    # A pair that differs only in position must not match.
    moved = json.loads(our_path.read_text(encoding="utf-8"))
    moved["violations"][0]["items"][0]["pos"]["x"] = 3.0001
    our_path.write_text(json.dumps(moved), encoding="utf-8")
    shifted = compare(jr, load_records(our_path), None)
    check("a moved position is a mismatch", not shifted["pass"])
    check("a moved position is one miss and one extra",
          shifted["missing"] == 1 and shifted["extra"] == 1)
    check("rounding can absorb it", compare(jr, load_records(our_path), 3)["pass"])

    # classify()
    check("classify exact", classify({"pass": True, "java_count": 5, "our_count": 5}) == "exact")
    check(
        "classify close",
        classify({"pass": False, "java_count": 1000, "our_count": 995}) == "close",
    )
    check(
        "classify far",
        classify({"pass": False, "java_count": 1000, "our_count": 500}) == "far",
    )

    java_path.unlink(missing_ok=True)
    our_path.unlink(missing_ok=True)

    for name in failures:
        print(f"FAIL {name}")
    print(f"self-test: {ran - len(failures)}/{ran} passed")
    return 1 if failures else 0


# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--fixture", help="compare one fixture by name")
    parser.add_argument("--all", action="store_true", help="sweep every oracle")
    parser.add_argument("--ours-dir", type=Path, default=DEFAULT_OURS_DIR)
    parser.add_argument("--fixtures-dir", type=Path, default=DEFAULT_FIXTURES_DIR)
    parser.add_argument(
        "--no-build",
        action="store_true",
        help="reuse cached reports in --ours-dir instead of re-running the bridge",
    )
    parser.add_argument(
        "--pos-decimals",
        type=int,
        default=None,
        help="round positions to N decimals before matching (default: exact)",
    )
    parser.add_argument(
        "--bridge-arg",
        action="append",
        default=[],
        metavar="FLAG",
        help="extra flag for the Rust bridge, repeatable (e.g. --bridge-arg --pin-frame-fix)",
    )
    parser.add_argument("--explain", type=int, default=0, metavar="N",
                        help="print the N most common unmatched keys per side")
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args(argv)

    if args.self_test:
        return self_test()
    if not args.all and not args.fixture:
        parser.error("one of --fixture, --all or --self-test is required")

    build = not args.no_build
    if build:
        if not GOLDEN_DIR.exists():
            print(f"no oracles at {GOLDEN_DIR}", file=sys.stderr)
            return 2
        try:
            build_bridge()
        except subprocess.CalledProcessError as error:
            print(f"cargo build failed: {error}", file=sys.stderr)
            return 2

    names = fixture_names() if args.all else [args.fixture]
    results = []
    for name in names:
        results.append(
            one_fixture(
                name,
                args.fixtures_dir,
                args.ours_dir,
                args.pos_decimals,
                build,
                args.bridge_arg,
            )
        )

    if args.json:
        payload = []
        for result in results:
            entry = {k: v for k, v in result.items() if not k.startswith("_")}
            entry.pop("missing_keys", None)
            entry.pop("extra_keys", None)
            payload.append(entry)
        print(json.dumps(payload, indent=2))
    else:
        print(summary_table(results))
        if args.explain:
            for result in results:
                if "_java_records" not in result or result["status"] == "exact":
                    continue
                print(f"\n{result['fixture']}:")
                for line in explain(
                    result, result["_java_records"], result["_our_records"], args.explain
                ):
                    print(line)

    return 0 if all(r.get("status") == "exact" for r in results) else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
