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

    def test_rounding_slack_absorbs_half_integer_diagonal_corners(self):
        # The jar's own sessions carry these: per-coordinate rounding of a
        # half-integer 45° corner (Issue187-processor.Z80.java.ses ×20).
        self.assertTrue(g4_kicad.segment_ok(6016, -6015, "fortyfive_degree"))
        self.assertTrue(g4_kicad.segment_ok(101, 100, "fortyfive_degree"))
        # …but a genuine off-angle segment is still caught.
        self.assertFalse(g4_kicad.segment_ok(1200, 3900, "fortyfive_degree"))
        self.assertFalse(g4_kicad.segment_ok(10, 8, "fortyfive_degree"))
        # The slack is a diagonal-rounding concept; 90° boards stay strict.
        self.assertFalse(g4_kicad.segment_ok(6016, -6015, "ninety_degree"))

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


def _violation(vtype, uuids):
    return {
        "type": vtype,
        "description": f"{vtype} violation",
        "items": [
            {"description": f"item {u}", "pos": {"x": 1.0, "y": 2.0}, "uuid": u}
            for u in uuids
        ],
    }


class ClassifyViolations(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = self.tmp.name

    def tearDown(self):
        self.tmp.cleanup()

    def test_pcb_uuids_reads_uuid_and_tstamp(self):
        pcb = os.path.join(self.dir, "b.kicad_pcb")
        _write(
            pcb,
            '(kicad_pcb (segment (uuid "aaaaaaaa-1111-1111-1111-111111111111"))'
            ' (via (tstamp "bbbbbbbb-2222-2222-2222-222222222222")))',
        )
        self.assertEqual(
            g4_kicad.pcb_uuids(pcb),
            {
                "aaaaaaaa-1111-1111-1111-111111111111",
                "bbbbbbbb-2222-2222-2222-222222222222",
            },
        )

    def test_violation_key_ignores_coordinate_jitter(self):
        a = _violation("clearance", ["u1", "u2"])
        b = _violation("clearance", ["u2", "u1"])
        b["items"][0]["pos"] = {"x": 1.0001, "y": 2.0002}
        self.assertEqual(g4_kicad.violation_key(a), g4_kicad.violation_key(b))

    def test_stable_split_into_debt_and_residual(self):
        before_uuids = {"old1", "old2"}
        before = {"violations": [], "unconnected_items": []}
        after = {
            "violations": [
                _violation("clearance", ["old1", "old2"]),  # fixture debt
                _violation("clearance", ["old1", "new1"]),  # residual
                _violation("copper_edge_clearance", ["old1", "new2"]),  # residual
            ],
            "unconnected_items": [],
        }
        # three identical runs -> no noise
        cls = g4_kicad.classify_violations([after, after, after], before, before_uuids)
        self.assertEqual(cls["raw_delta"], 3)
        self.assertEqual(cls["noise"], 0)
        self.assertEqual(cls["fixture_debt"], 1)
        self.assertEqual(cls["residual"], 2)
        self.assertEqual(len(cls["residual_violations"]), 2)

    def test_run_noise_is_dropped_from_residual(self):
        before_uuids = {"old1"}
        before = {"violations": [], "unconnected_items": []}
        flaky = _violation("shorting_items", ["old1", "new9"])
        steady = _violation("clearance", ["old1", "new1"])
        run1 = {"violations": [flaky, steady], "unconnected_items": []}
        run2 = {"violations": [steady], "unconnected_items": []}
        run3 = {"violations": [steady], "unconnected_items": []}
        cls = g4_kicad.classify_violations([run1, run2, run3], before, before_uuids)
        self.assertEqual(cls["raw_delta"], 2)
        self.assertEqual(cls["noise"], 1)
        self.assertEqual(cls["fixture_debt"], 0)
        self.assertEqual(cls["residual"], 1)
        self.assertEqual(cls["noise_examples"][0]["type"], "shorting_items")

    def test_item_without_uuid_is_never_fixture_debt(self):
        v = _violation("clearance", ["old1"])
        del v["items"][0]["uuid"]
        before = {"violations": [], "unconnected_items": []}
        after = {"violations": [v], "unconnected_items": []}
        cls = g4_kicad.classify_violations([after], before, {"old1"})
        self.assertEqual(cls["fixture_debt"], 0)
        self.assertEqual(cls["residual"], 1)

    def test_zone_fill_report_detects_changes(self):
        pre = os.path.join(self.dir, "pre.kicad_pcb")
        post = os.path.join(self.dir, "post.kicad_pcb")
        poly_a = "(filled_polygon (layer F.Cu) (pts (xy 0 0) (xy 1 0) (xy 1 1)))"
        poly_b = "(filled_polygon (layer F.Cu) (pts (xy 0 0) (xy 2 0) (xy 2 2)))"
        _write(pre, f"(kicad_pcb (zone {poly_a}))")
        _write(post, f"(kicad_pcb (zone {poly_b}))")
        rep = g4_kicad.zone_fill_report(pre, post)
        self.assertTrue(rep["changed"])
        self.assertEqual(rep["filled_polygons_before"], 1)
        self.assertEqual(rep["filled_polygons_after"], 1)
        self.assertEqual(rep["changed_polygons"], 2)
        _write(post, f"(kicad_pcb (zone {poly_a}))")
        self.assertFalse(g4_kicad.zone_fill_report(pre, post)["changed"])

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
