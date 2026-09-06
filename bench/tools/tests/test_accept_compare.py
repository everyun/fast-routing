#!/usr/bin/env python3
"""Hermetic tests for bench/tools/accept_compare.py."""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import accept_compare  # noqa: E402


class TestAcceptCompare(unittest.TestCase):
    def test_classify_board_same(self):
        base = {"fixture": "b1.dsn", "ours_unrouted": 0, "ours_violations": 0}
        ours = {"fixture": "b1.dsn", "ours_unrouted": 0, "ours_violations": 0}
        res = accept_compare.classify_board(base, ours)
        self.assertEqual(res["verdict"], "SAME")
        self.assertEqual(res["du"], 0)
        self.assertEqual(res["dv"], 0)
        self.assertEqual(res["added"], 0)

    def test_classify_board_better(self):
        base = {"fixture": "b1.dsn", "ours_unrouted": 2, "ours_violations": 1}
        ours = {"fixture": "b1.dsn", "ours_unrouted": 0, "ours_violations": 1}
        res = accept_compare.classify_board(base, ours)
        self.assertEqual(res["verdict"], "BETTER")
        self.assertEqual(res["du"], -2)

    def test_classify_board_worse_unrouted(self):
        base = {"fixture": "b1.dsn", "ours_unrouted": 0, "ours_violations": 0}
        ours = {"fixture": "b1.dsn", "ours_unrouted": 1, "ours_violations": 0}
        res = accept_compare.classify_board(base, ours)
        self.assertEqual(res["verdict"], "WORSE")
        self.assertEqual(res["du"], 1)

    def test_classify_board_worse_added_violations(self):
        base = {"fixture": "b1.dsn", "ours_unrouted": 0, "ours_violations": 2}
        ours = {"fixture": "b1.dsn", "ours_unrouted": 0, "ours_violations": 3}
        res = accept_compare.classify_board(base, ours)
        self.assertEqual(res["verdict"], "WORSE")
        self.assertEqual(res["added"], 1)

    def test_allow_deadline_noise_tolerates_timeout_noise(self):
        base = {"fixture": "t1.dsn", "ours_unrouted": 30, "ours_violations": 5, "java_class": "java_timeout"}
        ours = {"fixture": "t1.dsn", "ours_unrouted": 32, "ours_violations": 5, "java_class": "java_timeout"}
        # Without noise tolerance: WORSE
        res1 = accept_compare.classify_board(base, ours, allow_deadline_noise=0)
        self.assertEqual(res1["verdict"], "WORSE")

        # With noise tolerance 2: SAME
        res2 = accept_compare.classify_board(base, ours, allow_deadline_noise=2)
        self.assertEqual(res2["verdict"], "SAME")
        self.assertIn("deadline noise", res2["note"])

        # Violations still fail even on timeout board
        ours_violation = {"fixture": "t1.dsn", "ours_unrouted": 32, "ours_violations": 6, "java_class": "java_timeout"}
        res3 = accept_compare.classify_board(base, ours_violation, allow_deadline_noise=2)
        self.assertEqual(res3["verdict"], "WORSE")
        self.assertEqual(res3["added"], 1)

    def test_compare_gates_end_to_end(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            base_p = Path(tmpdir) / "base_gate.json"
            ours_p = Path(tmpdir) / "ours_gate.json"

            base_data = [
                {"fixture": "b1.dsn", "ours_unrouted": 0, "ours_violations": 0, "doors_mode": "default"},
                {"fixture": "b2.dsn", "ours_unrouted": 1, "ours_violations": 0, "doors_mode": "default"},
            ]
            ours_data = [
                {"fixture": "b1.dsn", "ours_unrouted": 0, "ours_violations": 0, "doors_mode": "full"},
                {"fixture": "b2.dsn", "ours_unrouted": 0, "ours_violations": 0, "doors_mode": "full"},
            ]

            base_p.write_text(json.dumps(base_data))
            ours_p.write_text(json.dumps(ours_data))

            rows, summary = accept_compare.compare_gates(ours_p, base_p)
            self.assertEqual(len(rows), 2)
            self.assertEqual(summary["better_count"], 1)
            self.assertEqual(summary["same_count"], 1)
            self.assertEqual(summary["worse_count"], 0)
            self.assertTrue(summary["can_merge"])
            self.assertTrue(summary["mode_mismatch"])  # full vs default


if __name__ == "__main__":
    unittest.main()
