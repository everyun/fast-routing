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


def _write_gate(path: Path, rows) -> Path:
    path.write_text(json.dumps(rows))
    return path


class TestDetectDoorsMode(unittest.TestCase):
    def test_full_mode(self):
        with tempfile.TemporaryDirectory() as tmp:
            p = _write_gate(Path(tmp) / "g.json", [{"fixture": "a.dsn", "doors_mode": "full"}])
            self.assertEqual(accept_compare.detect_doors_mode(p), "full")

    def test_default_mode(self):
        with tempfile.TemporaryDirectory() as tmp:
            p = _write_gate(Path(tmp) / "g.json", [{"fixture": "a.dsn", "doors_mode": "default"}])
            self.assertEqual(accept_compare.detect_doors_mode(p), "default")

    def test_missing_mode_is_unknown(self):
        with tempfile.TemporaryDirectory() as tmp:
            p = _write_gate(Path(tmp) / "g.json", [{"fixture": "a.dsn", "doors_mode": None}])
            self.assertEqual(accept_compare.detect_doors_mode(p), "unknown")

    def test_inconsistent_modes_are_unknown(self):
        with tempfile.TemporaryDirectory() as tmp:
            p = _write_gate(
                Path(tmp) / "g.json",
                [
                    {"fixture": "a.dsn", "doors_mode": "full"},
                    {"fixture": "b.dsn", "doors_mode": "default"},
                ],
            )
            self.assertEqual(accept_compare.detect_doors_mode(p), "unknown")


class TestInferSuite(unittest.TestCase):
    def test_oos_fixture(self):
        with tempfile.TemporaryDirectory() as tmp:
            p = _write_gate(Path(tmp) / "g.json", [{"fixture": "cla45-sensor-l6.dsn", "doors_mode": "full"}])
            self.assertEqual(accept_compare.infer_suite(p), "oos")

    def test_main_fixture(self):
        with tempfile.TemporaryDirectory() as tmp:
            p = _write_gate(Path(tmp) / "g.json", [{"fixture": "Issue039-bug-design.dsn", "doors_mode": "full"}])
            self.assertEqual(accept_compare.infer_suite(p), "main")


class TestFindLatestBaseline(unittest.TestCase):
    def _make_repo(self, tmp: str) -> Path:
        root = Path(tmp)
        sweeps = root / "bench" / "results" / "main-sweeps"
        # default-mode baseline at sha aaaa, full-mode baseline at sha bbbb
        for rel in (
            "aaaa/main/gate.json",
            "aaaa/oos/gate.json",
            "bbbb/full/main/gate.json",
            "bbbb/full/oos/gate.json",
        ):
            p = sweeps / rel
            p.parent.mkdir(parents=True, exist_ok=True)
            mode = "full" if rel.startswith("bbbb") else "default"
            p.write_text(json.dumps([{"fixture": "a.dsn", "doors_mode": mode}]))
        # make aaaa newer than bbbb so that ignoring mode would pick aaaa
        (sweeps / "aaaa").touch()
        (sweeps / "bbbb").touch()
        return root

    def test_full_mode_picks_full_baseline(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self._make_repo(tmp)
            got = accept_compare.find_latest_baseline(root, doors_mode="full", suite="main")
            self.assertEqual(got, root / "bench/results/main-sweeps/bbbb/full/main/gate.json")

    def test_default_mode_ignores_full_dir(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self._make_repo(tmp)
            got = accept_compare.find_latest_baseline(root, doors_mode="default", suite="main")
            self.assertEqual(got, root / "bench/results/main-sweeps/aaaa/main/gate.json")

    def test_oos_suite_resolution(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self._make_repo(tmp)
            got = accept_compare.find_latest_baseline(root, doors_mode="full", suite="oos")
            self.assertEqual(got, root / "bench/results/main-sweeps/bbbb/full/oos/gate.json")

    def test_missing_same_mode_baseline_raises(self):
        with tempfile.TemporaryDirectory() as tmp:
            empty = Path(tmp) / "empty"
            (empty / "bench" / "results" / "main-sweeps" / "aaaa" / "main").mkdir(parents=True)
            (empty / "bench" / "results" / "main-sweeps" / "aaaa" / "main" / "gate.json").write_text("[]")
            with self.assertRaises(FileNotFoundError) as cm:
                accept_compare.find_latest_baseline(empty, doors_mode="full", suite="main")
            self.assertIn("same-mode baseline", str(cm.exception))

    def test_unknown_mode_raises(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self._make_repo(tmp)
            with self.assertRaises(ValueError) as cm:
                accept_compare.find_latest_baseline(root, doors_mode="unknown", suite="main")
            self.assertIn("unknown", str(cm.exception))


class TestMainModeGuard(unittest.TestCase):
    """main() must refuse cross-mode comparisons instead of silently comparing."""

    def test_explicit_cross_mode_baseline_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = _write_gate(Path(tmp) / "base.json", [{"fixture": "a.dsn", "ours_unrouted": 0, "ours_violations": 0, "doors_mode": "default"}])
            ours = _write_gate(Path(tmp) / "ours.json", [{"fixture": "a.dsn", "ours_unrouted": 0, "ours_violations": 0, "doors_mode": "full"}])
            rc = accept_compare.main([str(ours), str(base)])
            self.assertEqual(rc, 2)

    def test_same_mode_explicit_baseline_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = _write_gate(Path(tmp) / "base.json", [{"fixture": "a.dsn", "ours_unrouted": 0, "ours_violations": 0, "doors_mode": "full"}])
            ours = _write_gate(Path(tmp) / "ours.json", [{"fixture": "a.dsn", "ours_unrouted": 0, "ours_violations": 0, "doors_mode": "full"}])
            rc = accept_compare.main([str(ours), str(base)])
            self.assertEqual(rc, 0)

    def test_auto_select_full_baseline_end_to_end(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            sweeps = root / "bench" / "results" / "main-sweeps" / "bbbb" / "full" / "main"
            sweeps.mkdir(parents=True)
            _write_gate(
                sweeps / "gate.json",
                [{"fixture": "a.dsn", "ours_unrouted": 1, "ours_violations": 0, "doors_mode": "full"}],
            )
            ours = _write_gate(
                root / "ours.json",
                [{"fixture": "a.dsn", "ours_unrouted": 1, "ours_violations": 0, "doors_mode": "full"}],
            )
            rc = accept_compare.main([str(ours), "--repo-root", str(root)])
            self.assertEqual(rc, 0)

    def test_auto_select_unknown_mode_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            ours = _write_gate(root / "ours.json", [{"fixture": "a.dsn", "ours_unrouted": 0, "ours_violations": 0, "doors_mode": None}])
            rc = accept_compare.main([str(ours), "--repo-root", str(root)])
            self.assertEqual(rc, 2)


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
