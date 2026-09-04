#!/usr/bin/env python3
"""Tests for bench/tools/g4_kicad.py — angle check, DRC counting, 4-tuple.

Hermetic: no KiCad, no fixtures, no router. Run:

    python3 -m unittest bench.tools.tests.test_g4_kicad
    python3 -m unittest discover -s bench/tools/tests
"""

from __future__ import annotations

import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import g4_kicad  # noqa: E402


class SegmentOk(unittest.TestCase):
    def test_axis_is_legal_under_45_and_90(self):
        self.assertTrue(g4_kicad.segment_ok(10, 0, "fortyfive_degree"))
        self.assertTrue(g4_kicad.segment_ok(0, 10, "fortyfive_degree"))
        self.assertTrue(g4_kicad.segment_ok(-4, 0, "ninety_degree"))
        self.assertTrue(g4_kicad.segment_ok(0, -4, "ninety_degree"))

    def test_diagonal_is_legal_under_45_illegal_under_90(self):
        self.assertTrue(g4_kicad.segment_ok(5, 5, "fortyfive_degree"))
        self.assertTrue(g4_kicad.segment_ok(5, -5, "fortyfive_degree"))
        self.assertFalse(g4_kicad.segment_ok(5, 5, "ninety_degree"))
        self.assertFalse(g4_kicad.segment_ok(5, -5, "ninety_degree"))

    def test_arbitrary_angle_is_illegal_unless_none(self):
        self.assertFalse(g4_kicad.segment_ok(10, 1, "fortyfive_degree"))
        self.assertFalse(g4_kicad.segment_ok(10, 1, "ninety_degree"))
        self.assertTrue(g4_kicad.segment_ok(10, 1, "none"))

    def test_zero_length_is_ignored_as_legal(self):
        self.assertTrue(g4_kicad.segment_ok(0, 0, "ninety_degree"))

    def test_default_snap_is_fortyfive(self):
        self.assertTrue(g4_kicad.segment_ok(3, 3, "fortyfive_degree"))
        self.assertFalse(g4_kicad.segment_ok(3, 1, "fortyfive_degree"))


def _write(path, text):
    with open(path, "w", encoding="utf-8") as f:
        f.write(text)


class SnapAndSes(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = self.tmp.name

    def tearDown(self):
        self.tmp.cleanup()

    def test_declared_snap_angle_is_read(self):
        dsn = os.path.join(self.dir, "a.dsn")
        _write(dsn, "(pcb x (structure (layer F.Cu (type signal)) (snap_angle ninety_degree)))")
        self.assertEqual(g4_kicad.snap_angle_of(dsn), "ninety_degree")

    def test_missing_snap_angle_falls_back_to_fortyfive(self):
        dsn = os.path.join(self.dir, "a.dsn")
        _write(dsn, "(pcb x (structure (layer F.Cu (type signal))))")
        self.assertEqual(g4_kicad.snap_angle_of(dsn), "fortyfive_degree")

    def test_ses_counts_illegal_segments_and_skips_zero_hops(self):
        ses = os.path.join(self.dir, "a.ses")
        # 0,0 -> 10,0 (axis, ok) ; 10,0 -> 10,0 (zero, skip) ; 10,0 -> 20,3 (bad under 90)
        _write(
            ses,
            "(session s (routes (network_out n "
            "(wire (path F.Cu 200 0 0 10 0 10 0 20 3)))))",
        )
        out = g4_kicad.angle_violations(ses, "ninety_degree")
        self.assertEqual(out["segments"], 2)
        self.assertEqual(out["angle_violations"], 1)
        self.assertEqual(len(out["examples"]), 1)

    def test_fortyfive_ses_with_only_octilinear_segments_is_clean(self):
        ses = os.path.join(self.dir, "a.ses")
        _write(
            ses,
            "(session s (routes (network_out n "
            "(wire (path F.Cu 200 0 0 10 0 20 10)))))",
        )
        out = g4_kicad.angle_violations(ses, "fortyfive_degree")
        self.assertEqual(out["angle_violations"], 0)
        self.assertEqual(out["segments"], 2)

    def test_quoted_numeric_layer_name_is_not_a_coordinate(self):
        # EasyEDA exports numeric layer names: (path "1" 6298 x1 y1 ...).
        # parse_sexp strips the quotes, so float-filtering would eat the layer
        # atom and shift every coordinate by one — easyeda-pcb1 showed 64
        # phantom violations that way. The true pairing is clean here.
        ses = os.path.join(self.dir, "a.ses")
        _write(
            ses,
            '(session s (routes (network_out n '
            '(wire (path "1" 200 0 0 10 0 20 10)))))',
        )
        out = g4_kicad.angle_violations(ses, "fortyfive_degree")
        self.assertEqual(out["segments"], 2)
        self.assertEqual(out["angle_violations"], 0)

    def test_imported_dsn_copper_is_classified_separately(self):
        # The DSN carries a pre-routed free-angle wire; the session re-emits it
        # verbatim (scaled by the resolution) plus one router-made bad segment.
        dsn = os.path.join(self.dir, "a.dsn")
        _write(
            dsn,
            "(pcb a (resolution um 10) (wiring "
            "(wire (path F.Cu 200 0 0 3 1)) (net N)))",
        )
        ses = os.path.join(self.dir, "a.ses")
        _write(
            ses,
            "(session s (routes (network_out n "
            "(wire (path F.Cu 2000 0 0 30 10 40 10)) "
            "(wire (path F.Cu 2000 0 0 9 1)))))",
        )
        out = g4_kicad.angle_violations(ses, "fortyfive_degree", dsn)
        self.assertEqual(out["angle_violations"], 2)
        self.assertEqual(out["angle_violations_imported"], 1)
        self.assertEqual(out["angle_violations_generated"], 1)

    def test_generated_bad_segments_gate_the_tuple(self):
        dsn = os.path.join(self.dir, "a.dsn")
        _write(
            dsn,
            "(pcb a (resolution um 10) (wiring "
            "(wire (path F.Cu 200 0 0 3 1)) (net N)))",
        )
        ses = os.path.join(self.dir, "a.rust.ses")
        _write(
            ses,
            "(session s (routes (network_out n "
            "(wire (path F.Cu 2000 0 0 30 10)))))",
        )
        row = g4_kicad.row_for(
            "a.dsn", dsn=dsn, ses=ses, pcb=None, gate_row=None,
            work_root=None, kicad_cli="", kicad_py="", skip_kicad=True,
        )
        # Only imported copper is off-angle: the tuple's angle number is 0.
        self.assertEqual(row["tuple"][2], 0)
        self.assertEqual(row["angle_violations_imported"], 1)


class DrcCount(unittest.TestCase):
    def test_unconnected_and_violations_are_separate(self):
        report = {
            "violations": [
                {"type": "clearance", "severity": "error"},
                {"type": "hole_clearance", "severity": "error"},
            ],
            "unconnected_items": [
                {"type": "unconnected_items", "items": [{}, {}]},
                {"type": "unconnected_items", "items": [{}, {}]},
            ],
        }
        c = g4_kicad.count_drc(report)
        self.assertEqual(c["violations"], 2)
        self.assertEqual(c["unconnected"], 2)

    def test_unconnected_type_inside_violations_is_not_a_drc_violation(self):
        report = {
            "violations": [
                {"type": "unconnected_items"},
                {"type": "clearance"},
            ],
            "unconnected_items": [],
        }
        c = g4_kicad.count_drc(report)
        self.assertEqual(c["violations"], 1)
        self.assertEqual(c["unconnected"], 1)

    def test_delta_is_after_minus_before(self):
        before = {"violations": [{"type": "clearance"}], "unconnected_items": [{}, {}]}
        after = {
            "violations": [{"type": "clearance"}, {"type": "clearance"}],
            "unconnected_items": [{}],
        }
        d = g4_kicad.drc_delta(before, after)
        self.assertEqual(d["drc_delta"], 1)
        self.assertEqual(d["unconnected_delta"], -1)

    def test_empty_report_is_zero(self):
        c = g4_kicad.count_drc({})
        self.assertEqual(c["violations"], 0)
        self.assertEqual(c["unconnected"], 0)


class FourTuple(unittest.TestCase):
    def test_shape(self):
        self.assertEqual(g4_kicad.four_tuple(3, 0, 1, 2), (3, 0, 1, 2))

    def test_row_without_ses_or_pcb_leaves_kicad_and_angles_empty(self):
        row = g4_kicad.row_for(
            "Board.dsn",
            dsn="/no/such.dsn",
            ses=None,
            pcb=None,
            gate_row={"ours_unrouted": 4, "added_violations": 0, "verdict": "pass"},
            work_root=None,
            kicad_cli="",
            kicad_py="",
            skip_kicad=True,
        )
        self.assertEqual(row["tuple"], (4, 0, None, None))
        self.assertTrue(row["kicad"]["skipped"])


if __name__ == "__main__":
    unittest.main()
