#!/usr/bin/env python3
"""Tests for `bench/tools/g21_judge.py` — the ROADMAP G2.1 gate itself.

The gate decides whether a milestone is met, so its own logic gets tested like
product code: every clause has a case that fails when the clause is removed,
and the two clauses that were *relaxed* (absolute `unrouted <= input`, and the
guard that keeps that relaxation honest) get both a board that must pass and a
board that must still fail.

No Rust and no fixtures in the loop: referee output is a plain dict, so these
run in milliseconds and cannot go stale when a router changes.

Run: `python3 -m unittest discover -s bench/tools/tests`
"""

from __future__ import annotations

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import g21_judge  # noqa: E402


def verdict(
    unrouted,
    violations,
    *,
    nets_with_copper=10,
    multi_pin=10,
    ses_errors=0,
):
    """One referee JSON object."""
    return {
        "violations": violations,
        "violations_both_user_fixed": 0,
        "violations_charged": violations,
        "incomplete": unrouted,
        "max_connections": 100,
        "score": 500.0,
        "wires_imported": 0,
        "vias_imported": 0,
        "ses_errors": ses_errors,
        "copper_items": nets_with_copper,
        "nets_with_copper": nets_with_copper,
        "nets_multi_pin": multi_pin,
        "overlay_carried_over": 0,
    }


def entry(java_class="java_finished", stability="stable", **kwargs):
    """One manifest fixture entry, with only the fields the gate reads."""
    out = {
        "name": "Board.dsn",
        "java": {"class": java_class},
        "stability": None if stability is None else {
            "class": stability,
            "started_unrouted": kwargs.get("started_unrouted"),
            "per_run": kwargs.get("per_run", {}),
        },
        "uncapped": kwargs.get("uncapped"),
    }
    return out


def judge(name="Board.dsn", *, e=None, base=None, ours=None, java=None, run=None):
    return g21_judge.judge_one(
        name, e or entry(), base, ours, java, run or {}
    )


class GateSelection(unittest.TestCase):
    def test_java_finished_and_stable_with_a_reference_is_comparative(self):
        row = judge(
            base=verdict(100, 5), ours=verdict(3, 5), java=verdict(3, 5)
        )
        self.assertEqual(row["gate"], "comparative")
        self.assertEqual(row["verdict"], "pass")

    def test_java_finished_but_unstable_drops_to_the_absolute_gate(self):
        # The point of the stability sweep: a number that moves between runs is
        # not something to be measured against.
        row = judge(
            e=entry(stability="unstable"),
            base=verdict(100, 5),
            ours=verdict(3, 5),
            java=verdict(3, 5),
        )
        self.assertEqual(row["gate"], "absolute")

    def test_java_finished_without_a_reference_session_drops_to_absolute(self):
        row = judge(base=verdict(100, 5), ours=verdict(3, 5), java=None)
        self.assertEqual(row["gate"], "absolute")
        self.assertIn("its session is absent", row["reason"])

    def test_a_timeout_board_is_absolute_even_when_stable(self):
        row = judge(
            e=entry(java_class="java_timeout", stability="no_result"),
            base=verdict(100, 5),
            ours=verdict(30, 5),
        )
        self.assertEqual(row["gate"], "absolute")


class ComparativeGate(unittest.TestCase):
    def test_more_added_violations_than_the_reference_fails(self):
        row = judge(base=verdict(100, 0), ours=verdict(0, 1), java=verdict(0, 0))
        self.assertEqual(row["verdict"], "FAIL")
        self.assertIn("added 1 violations", row["reason"])

    def test_one_connection_behind_the_reference_fails(self):
        row = judge(base=verdict(100, 5), ours=verdict(2, 5), java=verdict(1, 5))
        self.assertEqual(row["verdict"], "FAIL")
        self.assertIn("unrouted 2 > reference 1", row["reason"])

    def test_the_reference_adding_violations_raises_our_allowance(self):
        # `added <= java_added`, not `added == 0`: on a board Java dirties too,
        # matching it is passing.
        row = judge(base=verdict(100, 5), ours=verdict(3, 7), java=verdict(3, 7))
        self.assertEqual(row["verdict"], "pass")


class AbsoluteGate(unittest.TestCase):
    def timeout_entry(self, **kwargs):
        return entry(java_class="java_timeout", stability="no_result", **kwargs)

    def test_any_added_violation_fails(self):
        row = judge(
            e=self.timeout_entry(uncapped={"started_unrouted": 100,
                                           "uncapped_unrouted": 2}),
            base=verdict(100, 0),
            ours=verdict(30, 2),
        )
        self.assertEqual(row["verdict"], "FAIL")
        self.assertIn("added 2 violations", row["reason"])

    def test_going_backwards_from_the_input_fails(self):
        row = judge(
            e=self.timeout_entry(),
            base=verdict(100, 0),
            ours=verdict(101, 0),
        )
        self.assertEqual(row["verdict"], "FAIL")
        self.assertIn("above the input's 100", row["reason"])

    def test_being_worse_than_a_present_reference_fails(self):
        row = judge(
            e=self.timeout_entry(),
            base=verdict(100, 0),
            ours=verdict(40, 0),
            java=verdict(20, 0),
        )
        self.assertEqual(row["verdict"], "FAIL")
        self.assertIn("unrouted 40 > reference 20", row["reason"])


class AntiDegeneracyGuard(unittest.TestCase):
    """`unrouted <= input` is only safe because of this guard."""

    def test_standing_still_fails_when_the_reference_moved_the_board(self):
        row = judge(
            e=entry(java_class="java_timeout", stability="no_result",
                    uncapped={"started_unrouted": 981, "uncapped_unrouted": 2}),
            base=verdict(100, 0),
            ours=verdict(100, 0),
        )
        self.assertEqual(row["verdict"], "FAIL")
        self.assertIn("anti-degeneracy", row["reason"])
        self.assertTrue(row["reference_made_progress"])

    def test_an_empty_session_fails_when_the_reference_moved_the_board(self):
        row = judge(
            e=entry(java_class="java_timeout", stability="no_result",
                    uncapped={"started_unrouted": 981, "uncapped_unrouted": 2}),
            base=verdict(100, 0),
            ours=verdict(100, 0, nets_with_copper=0),
        )
        self.assertEqual(row["verdict"], "FAIL")
        self.assertIn("no copper for any multi-pin net", row["reason"])

    def test_no_evidence_at_all_still_demands_progress(self):
        # Silence must not be a way through.
        row = judge(
            e=entry(java_class="java_unknown", stability=None),
            base=verdict(100, 0),
            ours=verdict(100, 0),
        )
        self.assertEqual(row["verdict"], "FAIL")
        self.assertIsNone(row["reference_made_progress"])

    def test_issue229_shape_the_reference_regressed_the_board(self):
        # 20 unrouted in, 20 unrouted out, 0 added violations; the reference's
        # own session is 64 unrouted, i.e. worse than the input.
        row = judge(
            e=entry(java_class="java_degenerate", stability="unmeasured"),
            base=verdict(20, 0),
            ours=verdict(20, 0, nets_with_copper=30, multi_pin=26),
            java=verdict(64, 0),
        )
        self.assertEqual(row["verdict"], "pass")
        self.assertFalse(row["reference_made_progress"])

    def test_issue208_shape_nobody_routes_it(self):
        # No reference session; the uncapped run spent its whole budget in the
        # routing stage without finishing pass 1.
        row = judge(
            e=entry(
                java_class="java_timeout",
                stability="no_result",
                started_unrouted=378,
                per_run={"r1": {"unrouted": None, "routing_started": True}},
                uncapped={
                    "started_unrouted": 378,
                    "uncapped_unrouted": None,
                    "status": "hit_cap",
                    "passes_run": 0,
                    "uncapped_wall_s": 7200.4,
                },
            ),
            base=verdict(378, 17),
            ours=verdict(378, 17, nets_with_copper=0, multi_pin=299),
        )
        self.assertEqual(row["verdict"], "pass")
        self.assertFalse(row["reference_made_progress"])
        self.assertIn("without finishing pass 1", row["progress_evidence"])

    def test_a_board_with_no_multi_pin_nets_is_not_asked_for_copper(self):
        row = judge(
            e=entry(java_class="java_degenerate", stability="no_result"),
            base=verdict(0, 0, nets_with_copper=0, multi_pin=0),
            ours=verdict(0, 0, nets_with_copper=0, multi_pin=0),
        )
        self.assertEqual(row["verdict"], "pass")


class ProgressEvidence(unittest.TestCase):
    def test_a_reference_session_outranks_the_logs(self):
        made, why = g21_judge.reference_made_progress(
            entry(uncapped={"started_unrouted": 100, "uncapped_unrouted": 1}),
            verdict(50, 0),
            verdict(60, 0),
        )
        self.assertFalse(made)
        self.assertIn("reference session", why)

    def test_stability_runs_are_compared_with_their_own_starting_count(self):
        made, why = g21_judge.reference_made_progress(
            entry(
                stability="stable",
                started_unrouted=33,
                per_run={"r1": {"unrouted": 0}, "r2": {"unrouted": 0}},
            ),
            verdict(999, 0),  # a referee count, deliberately far from Java's
            None,
        )
        self.assertTrue(made)
        self.assertIn("their own starting count", why)

    def test_a_blocked_uncapped_run_is_not_evidence_of_anything(self):
        made, _ = g21_judge.reference_made_progress(
            entry(
                java_class="java_timeout",
                stability=None,
                uncapped={"status": "blocked", "passes_run": 0,
                          "started_unrouted": None, "uncapped_unrouted": None},
            ),
            verdict(50, 0),
            None,
        )
        self.assertIsNone(made)


class HardFailures(unittest.TestCase):
    def test_a_session_that_does_not_read_back_fails_whatever_its_numbers(self):
        row = judge(
            base=verdict(100, 5), ours=verdict(0, 0, ses_errors=1), java=verdict(3, 5)
        )
        self.assertEqual(row["verdict"], "FAIL")
        self.assertIn("does not read back cleanly", row["reason"])

    def test_no_session_at_all_fails(self):
        row = judge(base=verdict(100, 5), ours=None, run={"timed_out": True})
        self.assertEqual(row["verdict"], "FAIL")
        self.assertIn("timed out", row["reason"])

    def test_a_file_that_is_not_a_board_is_nobody_s_problem(self):
        row = judge(
            e=entry(java_class="java_degenerate", stability="no_result"),
            base={"error": "import: NotADsnFile"},
            ours=None,
        )
        self.assertEqual(row["verdict"], "pass")


class Fidelity(unittest.TestCase):
    def test_a_gap_between_the_in_process_line_and_the_judged_board_is_reported(self):
        rows = [
            {
                "fixture": "Board.dsn",
                "self_referee": "Referee: 16 clearance violations, 5 of 203 "
                                "connections incomplete, score 959.60/1000",
                "ours_unrouted": 8,
                "ours_violations_raw": 14,
            }
        ]
        self.assertEqual(len(g21_judge.fidelity_gaps(rows)), 1)

    def test_agreement_reports_nothing(self):
        rows = [
            {
                "fixture": "Board.dsn",
                "self_referee": "Referee: 16 clearance violations, 5 of 203 "
                                "connections incomplete, score 959.60/1000",
                "ours_unrouted": 5,
                "ours_violations_raw": 16,
            }
        ]
        self.assertEqual(g21_judge.fidelity_gaps(rows), [])


if __name__ == "__main__":
    unittest.main(verbosity=2)
