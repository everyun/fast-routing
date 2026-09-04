#!/usr/bin/env python3
"""Hermetic tests for `bench/tools/spec_lint.py`.

A pair of markdown files that agree on a constant must be silent; the same
constant with two numeric spellings of one value must also be silent; a
real disagreement (20000 vs 500) must surface as a conflict. No specs
from the tree are required.

Run: `python3 -m unittest discover -s bench/tools/tests`
"""

from __future__ import annotations

import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import spec_lint as sl  # noqa: E402


AGREE_A = """# Spec A

| Constant | Value | Line |
|---|---|---|
| `FANOUT_COST_CONST` | `20000` | :118 |
| `TRACE_WIDTH_TOLERANCE` | 2 | :30 |
"""

AGREE_B = """# Spec B

| Knob | Value | Where |
|---|---|---|
| `FANOUT_COST_CONST` | 20000.0d | MazeSearchAlgo |
| `AutorouteEngine.TRACE_WIDTH_TOLERANCE` | **2** | AutorouteEngine.java:33 |
"""

CONFLICT_A = """# Spec A

| Constant | Value |
|---|---|
| `minEscapeLengthMm` | 2.5 |
"""

CONFLICT_B = """# Spec B

## Release-jar conformance (2.3.0 @2d4de019)

Defaults: `minEscapeLengthMm` = 500 (the v2-tree table).
"""

INNER_ASSIGN = """# Spec C

## Release-jar conformance (2.3.0 @2d4de019)

Real defaults are `minEscapeLengthMm = 2.5`, `maxEscapeLengthMm = 4.5`.
"""

PERF_ROW = """# perf

| Constant | Value | Site |
|---|---|---|
| fanout escape gate | `maxEscapeLengthMm` default **3000**, `minEscapeLengthMm` default **500** | MazeSearchEngine |
"""

CONFORMANCE_RELEASE_COL = """# Spec

| constant | spec value | release value | verdict |
|---|---|---|---|
| `MAX_RIPUP_COSTS` | `Integer.MAX_VALUE / 100` | 21474836 | ok |
"""

OTHER = """# Other spec

| Constant | Value |
|---|---|
| `MAX_RIPUP_COSTS` | `Integer.MAX_VALUE / 100` |
"""


def write_specs(files: dict[str, str]) -> str:
    tmp = tempfile.mkdtemp(prefix="spec-lint-")
    for name, text in files.items():
        with open(os.path.join(tmp, name), "w", encoding="utf-8") as f:
            f.write(text)
    return tmp


class Agreement(unittest.TestCase):
    def test_same_constant_same_value_is_not_a_conflict(self):
        d = write_specs({"a.md": AGREE_A, "b.md": AGREE_B})
        result = sl.lint_dir(d)
        self.assertTrue(result["ok"])
        self.assertEqual(result["conflicts"], [])

    def test_integer_max_value_over_100_matches_the_bytecode_int(self):
        d = write_specs({"a.md": CONFORMANCE_RELEASE_COL, "b.md": OTHER})
        result = sl.lint_dir(d)
        names = {c["name"] for c in result["conflicts"]}
        self.assertNotIn("MAX_RIPUP_COSTS", names, result["conflicts"])


class Disagreement(unittest.TestCase):
    def test_cross_spec_numeric_mismatch_is_a_conflict(self):
        d = write_specs({"a.md": CONFLICT_A, "b.md": CONFLICT_B})
        result = sl.lint_dir(d)
        names = {c["name"] for c in result["conflicts"]}
        self.assertIn("minEscapeLengthMm", names)
        conflict = next(c for c in result["conflicts"] if c["name"] == "minEscapeLengthMm")
        comparables = {v["comparable"] for v in conflict["values"]}
        self.assertEqual(comparables, {"2.5", "500"})

    def test_default_phrase_in_a_value_cell_is_harvested(self):
        d = write_specs({"a.md": CONFLICT_A, "p.md": PERF_ROW})
        result = sl.lint_dir(d)
        names = {c["name"] for c in result["conflicts"]}
        self.assertIn("minEscapeLengthMm", names)

    def test_inner_backtick_assignment_conflicts_with_default_phrase(self):
        d = write_specs({"c.md": INNER_ASSIGN, "p.md": PERF_ROW})
        result = sl.lint_dir(d)
        names = {c["name"] for c in result["conflicts"]}
        self.assertIn("minEscapeLengthMm", names)
        self.assertIn("maxEscapeLengthMm", names)

    def test_strict_flag_exits_one(self):
        d = write_specs({"a.md": CONFLICT_A, "b.md": CONFLICT_B})
        rc = sl.main(["--dir", d, "--strict"])
        self.assertEqual(rc, 1)

    def test_default_exit_zero_even_with_conflicts(self):
        d = write_specs({"a.md": CONFLICT_A, "b.md": CONFLICT_B})
        rc = sl.main(["--dir", d])
        self.assertEqual(rc, 0)


class Parsing(unittest.TestCase):
    def test_canonical_name_strips_class_prefix(self):
        self.assertEqual(
            sl.canonical_name("AutorouteEngine.TRACE_WIDTH_TOLERANCE"),
            "TRACE_WIDTH_TOLERANCE",
        )

    def test_normalize_underscored_float_suffix(self):
        display, comparable = sl.normalize_value("`5_000_000.0f`")
        self.assertEqual(comparable, "5000000")

    def test_ms_unit_collapses_to_the_number(self):
        self.assertEqual(sl.parse_scalar("1000 ms"), "1000")

    def test_prose_value_is_not_scalar(self):
        self.assertIsNone(sl.parse_scalar("20 / 5 / (ctrl)"))

    def test_missing_dir_is_not_ok(self):
        result = sl.lint_dir("/no/such/spec/dir")
        self.assertFalse(result["ok"])


if __name__ == "__main__":
    unittest.main()
