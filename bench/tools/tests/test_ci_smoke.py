#!/usr/bin/env python3
"""Hermetic tests for `bench/tools/ci_smoke.py` evaluate()."""

from __future__ import annotations

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import ci_smoke  # noqa: E402


def verdict(unrouted, charged, *, ses_errors=0):
    return {
        "violations_charged": charged,
        "incomplete": unrouted,
        "ses_errors": ses_errors,
    }


class Evaluate(unittest.TestCase):
    def test_zero_added_and_unrouted_at_ceiling_passes(self):
        row = ci_smoke.evaluate(verdict(10, 5), verdict(0, 5), unrouted_max=0)
        self.assertTrue(row["ok"], row)
        self.assertEqual(row["added_violations"], 0)
        self.assertEqual(row["unrouted"], 0)

    def test_added_violation_fails(self):
        row = ci_smoke.evaluate(verdict(10, 5), verdict(0, 6), unrouted_max=3)
        self.assertFalse(row["ok"])
        self.assertEqual(row["added_violations"], 1)

    def test_unrouted_above_baseline_fails(self):
        row = ci_smoke.evaluate(verdict(10, 0), verdict(4, 0), unrouted_max=3)
        self.assertFalse(row["ok"])
        self.assertTrue(any("unrouted" in r for r in row["reasons"]))

    def test_unrouted_equal_to_baseline_passes(self):
        row = ci_smoke.evaluate(verdict(10, 0), verdict(3, 0), unrouted_max=3)
        self.assertTrue(row["ok"], row)

    def test_ses_errors_fail(self):
        row = ci_smoke.evaluate(
            verdict(10, 0), verdict(0, 0, ses_errors=2), unrouted_max=0
        )
        self.assertFalse(row["ok"])

    def test_missing_session_fails(self):
        row = ci_smoke.evaluate(verdict(10, 0), {"error": "no session produced"}, 0)
        self.assertFalse(row["ok"])


if __name__ == "__main__":
    unittest.main()
