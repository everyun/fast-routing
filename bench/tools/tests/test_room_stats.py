#!/usr/bin/env python3
"""Tests for `bench/tools/room_stats.py`.

The tool's whole job is to read two differently-spelled logs — our `FRROOM`
records and the lab jar's `COMPLETE_ROOM` / `compare_trace_insert_segment_raw`
TRACE lines — into the *same* table, so the tests are written as a pair: the
same decomposition, spelled both ways, must summarise identically.

Run: `python3 -m unittest discover -s bench/tools/tests`
"""

from __future__ import annotations

import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

import room_stats as rs  # noqa: E402

OURS = """\
FRROOM input, net=76, layer=0, room_bounds=empty, contained_bounds=[(5,5)..(5,5)], from_door_bounds=null
FRROOM added, net=76, layer=0, bounds=[(0,0)..(100,100)], area=10000.0, corners=4
FRROOM input, net=76, layer=0, room_bounds=unbounded, contained_bounds=[(0,0)..(0,0)], from_door_bounds=null
FRROOM added, net=76, layer=0, bounds=[(100,0)..(200,10)], area=1000.0, corners=4
FRROOM added, net=77, layer=1, bounds=[(0,0)..(50,50)], area=2500.0, corners=4
FRROOM located, net=76, hops=9, corners=4, items=1
FRROOM located, net=77, hops=3, corners=2, items=1
"""

JAVA = """\
2026-08-29 22:34:39.548 TRACE  COMPLETE_ROOM input, net=76, layer=0, room_bounds=null, contained_bounds=[(5,5)..(5,5)], from_door_bounds=null, ignore_object=null
2026-08-29 22:34:39.549 TRACE  COMPLETE_ROOM added, net=76, layer=0, bounds=[(0,0)..(100,100)]
2026-08-29 22:34:39.550 TRACE  COMPLETE_ROOM input, net=76, layer=0, room_bounds=[(0,0)..(1,1)], contained_bounds=[(0,0)..(0,0)], from_door_bounds=null, ignore_object=null
2026-08-29 22:34:39.551 TRACE  COMPLETE_ROOM added, net=76, layer=0, bounds=[(100,0)..(200,10)]
2026-08-29 22:34:39.552 TRACE  COMPLETE_ROOM added, net=77, layer=1, bounds=[(0,0)..(50,50)]
2026-08-29 22:34:39.553 TRACE  compare_trace_insert_segment_raw net=76, layer=0, i=1, decision=ADVANCE
2026-08-29 22:34:39.554 TRACE  compare_trace_insert_segment_raw net=76, layer=0, i=2, decision=ADVANCE
2026-08-29 22:34:39.555 TRACE  compare_trace_insert_segment_raw net=76, layer=0, i=3, decision=ADVANCE
2026-08-29 22:34:39.556 TRACE  compare_trace_insert_segment_raw net=77, layer=0, i=1, decision=ADVANCE
"""


def write(text: str) -> str:
    handle = tempfile.NamedTemporaryFile("w", suffix=".log", delete=False)
    handle.write(text)
    handle.close()
    return handle.name


class ParseTest(unittest.TestCase):
    def setUp(self) -> None:
        self.ours_path, self.java_path = write(OURS), write(JAVA)
        self.addCleanup(os.unlink, self.ours_path)
        self.addCleanup(os.unlink, self.java_path)
        self.ours = rs.parse(self.ours_path)
        self.java = rs.parse(self.java_path)

    def test_both_spellings_yield_the_same_rooms(self):
        self.assertEqual(len(self.ours.rooms), 3)
        self.assertEqual(
            [(r.net, r.layer, r.width, r.height) for r in self.ours.rooms],
            [(r.net, r.layer, r.width, r.height) for r in self.java.rooms],
        )

    def test_both_spellings_yield_the_same_input_count(self):
        self.assertEqual(self.ours.inputs, 2)
        self.assertEqual(self.java.inputs, 2)

    def test_added_lines_are_not_miscounted_as_inputs(self):
        # `added` and `input` share the `, net=` prefix; a sloppy reader that
        # tests the substring first would double-count.
        self.assertEqual(self.ours.inputs + len(self.ours.rooms), 5)

    def test_java_segment_runs_reconstruct_our_chain_corner_counts(self):
        # Java prints one line per *segment*, `i` restarting at 1 per trace, so
        # a run ending at `i=3` is a 4-corner chain — the number our `located`
        # record carries directly.
        self.assertEqual(self.ours.chains, [4, 2])
        self.assertEqual(self.java.chains, [4, 2])

    def test_a_run_is_split_when_the_index_stops_ascending(self):
        log = write(
            "TRACE compare_trace_insert_segment_raw net=1, layer=0, i=1\n"
            "TRACE compare_trace_insert_segment_raw net=1, layer=0, i=2\n"
            "TRACE compare_trace_insert_segment_raw net=1, layer=0, i=1\n"
        )
        self.addCleanup(os.unlink, log)
        self.assertEqual(rs.parse(log).chains, [3, 2])

    def test_a_run_is_split_when_the_net_changes(self):
        log = write(
            "TRACE compare_trace_insert_segment_raw net=1, layer=0, i=1\n"
            "TRACE compare_trace_insert_segment_raw net=2, layer=0, i=2\n"
        )
        self.addCleanup(os.unlink, log)
        self.assertEqual(rs.parse(log).chains, [2, 3])


class SummaryTest(unittest.TestCase):
    def setUp(self) -> None:
        self.path = write(OURS)
        self.addCleanup(os.unlink, self.path)
        self.log = rs.parse(self.path)

    def test_area_is_the_bounding_box_area_both_sides_can_report(self):
        stats = rs.summarise(self.log, sliver=rs.DEFAULT_SLIVER)
        # 100×100, 100×10, 50×50 → median 2500.
        self.assertEqual(stats["area_p50"], 2500)
        self.assertEqual(stats["rooms"], 3)
        self.assertEqual(stats["nets"], 2)

    def test_a_sliver_is_thin_in_either_axis(self):
        stats = rs.summarise(self.log, sliver=20)
        # Only the 100×10 room is thinner than 20 in an axis.
        self.assertAlmostEqual(stats["sliver_frac"], 1 / 3)
        self.assertEqual(rs.summarise(self.log, sliver=1)["sliver_frac"], 0.0)

    def test_corner_statistics_come_from_the_located_records(self):
        stats = rs.summarise(self.log, sliver=rs.DEFAULT_SLIVER)
        self.assertEqual(stats["chains"], 2)
        self.assertEqual(stats["corners_max"], 4)
        self.assertEqual(stats["corners_mean"], 3.0)

    def test_an_empty_log_summarises_without_dividing_by_zero(self):
        empty = write("nothing to see here\n")
        self.addCleanup(os.unlink, empty)
        stats = rs.summarise(rs.parse(empty), sliver=rs.DEFAULT_SLIVER)
        self.assertEqual(stats["rooms"], 0)
        self.assertEqual(stats["inputs"], 0)


class CommandTest(unittest.TestCase):
    def test_compare_runs_end_to_end(self):
        ours, java = write(OURS), write(JAVA)
        self.addCleanup(os.unlink, ours)
        self.addCleanup(os.unlink, java)
        self.assertEqual(rs.main(["compare", ours, java]), 0)
        self.assertEqual(rs.main(["summary", ours, java]), 0)
        self.assertEqual(rs.main(["nets", ours]), 0)


if __name__ == "__main__":
    unittest.main()
