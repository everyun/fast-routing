#!/usr/bin/env python3
"""Self-tests for the uncapped-run parser and the campaign tables.

The write-up in measurement-campaign.md cites four numbers per board that all
come out of `manifest.parse_uncapped_log`, and one classification (`status`)
that decides whether a wall time may be cited as T_java(complete) at all. That
classification is the thing worth pinning: two of the fourteen boards spent
their whole 2 h on a modal confirmation dialog, and a parser that called those
"routing took 7200 s" would put a fabricated number into a ROADMAP gate.

    python3 bench/analysis/test_campaign.py
"""

from __future__ import annotations

import os
import sys
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
BENCH_DIR = os.path.dirname(HERE)
sys.path.insert(0, BENCH_DIR)
sys.path.insert(0, HERE)

import manifest as fixture_manifest  # noqa: E402
import campaign  # noqa: E402

TIME_FOOTER = """      459.75 real       517.38 user        31.30 sys
          3712696320  maximum resident set size
"""

COMPLETED = """2026-08-28 05:13:28.240 INFO   [X] Auto-routing stage started on board 'b' for 146 unrouted items.
2026-08-28 05:14:09.526 INFO   [X] Auto-routing pass #1 on board 'b' was completed in 40.90 seconds with the score of 663.07 (73 unrouted and 511 violations), using 40.86 CPU seconds and the job allocated 301.61 GB of memory so far.
2026-08-28 05:14:39.548 INFO   [X] Auto-routing pass #2 on board 'c' was completed in 29.62 seconds with the score of 678.46 (65 unrouted and 511 violations), using 69.71 CPU seconds and the job allocated 504.20 GB of memory so far.
2026-08-28 05:20:55.530 INFO   [X] Auto-routing stage completed: started with 146 unrouted nets, completed in 447.39 seconds, final score: 699.61 (54 unrouted and 511 violations), using 441.86 total CPU seconds, 3265.80 GB total allocated, and 1893.6 MB peak heap usage.
""" + TIME_FOOTER

BLOCKED = """2026-08-28 05:13:18.969 INFO   [X] Loading board file 'Issue029-hw48na.dsn'...
2026-08-28 05:13:19.665 INFO   Opening 'Issue029-hw48na.rules'...
     7200.39 real        12.29 user         4.81 sys
           858472448  maximum resident set size
"""

HIT_CAP = """2026-08-28 06:33:07.833 INFO   [X] Auto-routing stage started on board 'b' for 378 unrouted items.
     7200.38 real      7329.02 user       122.50 sys
          3673047040  maximum resident set size
"""

IN_FLIGHT = """2026-08-28 06:33:07.833 INFO   [X] Auto-routing stage started on board 'b' for 378 unrouted items.
"""


class TestParseUncappedLog(unittest.TestCase):
    def test_completed_run_reports_final_stage_numbers(self):
        rec = fixture_manifest.parse_uncapped_log(COMPLETED)
        self.assertEqual(rec["status"], "completed")
        self.assertIs(rec["hit_cap"], False)
        self.assertEqual(rec["uncapped_wall_s"], 459.75)
        self.assertEqual(rec["uncapped_unrouted"], 54)
        self.assertEqual(rec["uncapped_violations"], 511)
        self.assertEqual(rec["score"], 699.61)
        self.assertEqual(rec["passes_run"], 2)
        self.assertEqual(rec["stages"], {"auto-routing": 447.39})
        self.assertEqual(rec["routing_s"], 447.39)

    def test_final_line_wins_over_the_last_pass_line(self):
        # The last pass scored 678.46/65u; the stage line is authoritative.
        rec = fixture_manifest.parse_uncapped_log(COMPLETED)
        self.assertEqual(rec["uncapped_unrouted"], 54)

    def test_pass_lines_are_the_fallback_when_no_stage_line(self):
        truncated = COMPLETED.split("Auto-routing stage completed")[0] + TIME_FOOTER
        rec = fixture_manifest.parse_uncapped_log(truncated)
        self.assertEqual(rec["uncapped_unrouted"], 65)
        self.assertEqual(rec["score"], 678.46)
        self.assertIsNone(rec["routing_s"])

    def test_dialog_hang_is_blocked_not_a_routing_time(self):
        rec = fixture_manifest.parse_uncapped_log(BLOCKED)
        self.assertEqual(rec["status"], "blocked")
        self.assertIs(rec["hit_cap"], True)
        self.assertEqual(rec["stages_started"], [])
        self.assertEqual(rec["rules_sidecar"], "Issue029-hw48na.rules")
        # Nothing to report as a result — a consumer must not read 0 here.
        self.assertIsNone(rec["uncapped_unrouted"])
        self.assertIsNone(rec["score"])

    def test_cap_while_routing_is_hit_cap(self):
        rec = fixture_manifest.parse_uncapped_log(HIT_CAP)
        self.assertEqual(rec["status"], "hit_cap")
        self.assertIs(rec["hit_cap"], True)
        self.assertEqual(rec["stages_started"], ["auto-routing"])

    def test_missing_time_footer_is_in_flight(self):
        rec = fixture_manifest.parse_uncapped_log(IN_FLIGHT)
        self.assertEqual(rec["status"], "in_flight")
        self.assertIsNone(rec["hit_cap"])
        self.assertIsNone(rec["uncapped_wall_s"])

    def test_status_is_always_one_of_the_declared_values(self):
        for text in (COMPLETED, BLOCKED, HIT_CAP, IN_FLIGHT, ""):
            rec = fixture_manifest.parse_uncapped_log(text)
            self.assertIn(rec["status"], fixture_manifest.UNCAPPED_STATUSES)

    def test_empty_log_does_not_raise(self):
        rec = fixture_manifest.parse_uncapped_log("")
        self.assertEqual(rec["passes_run"], 0)
        self.assertEqual(rec["stages"], {})


class TestStabilitySweep(unittest.TestCase):
    """`stability.class` picks which G2.1 gate a board gets, so it is held to
    the same standard as `uncapped.status`: a parser that called a board
    "stable" on no evidence would hand the comparative gate a number that does
    not reproduce."""

    def runs(self, *texts):
        by_index = {
            index + 1: fixture_manifest.parse_stability_run(text)
            for index, text in enumerate(texts)
        }
        logs = {index: f"r{index}.log" for index in by_index}
        return fixture_manifest.summarise_stability(by_index, logs)

    def test_three_agreeing_runs_are_stable(self):
        out = self.runs(COMPLETED, COMPLETED, COMPLETED)
        self.assertEqual(out["class"], "stable")
        self.assertEqual(out["unrouted"], 54)
        self.assertEqual(out["violations"], 511)
        self.assertEqual(out["started_unrouted"], 146)

    def test_one_disagreeing_run_makes_the_board_unstable(self):
        drifted = COMPLETED.replace(
            "(54 unrouted and 511 violations)", "(55 unrouted and 511 violations)"
        )
        out = self.runs(COMPLETED, COMPLETED, drifted)
        self.assertEqual(out["class"], "unstable")
        self.assertIsNone(out["unrouted"])
        self.assertIn("r3=55u/511v", out["reason"])

    def test_finishing_only_sometimes_is_unstable_not_stable(self):
        out = self.runs(COMPLETED, HIT_CAP, COMPLETED)
        self.assertEqual(out["class"], "unstable")
        self.assertEqual(out["scored_runs"], 2)

    def test_runs_that_all_hit_the_cap_are_no_result_not_stable(self):
        out = self.runs(HIT_CAP, HIT_CAP, HIT_CAP)
        self.assertEqual(out["class"], "no_result")
        self.assertIn("killed at the cap", out["reason"])
        # Java's own starting count survives even when nothing else does; the
        # anti-degeneracy guard is the caller that needs it.
        self.assertEqual(out["started_unrouted"], 378)

    def test_a_board_that_never_starts_routing_says_so(self):
        out = self.runs(BLOCKED, BLOCKED, BLOCKED)
        self.assertEqual(out["class"], "no_result")
        self.assertIn("no routing stage ever started", out["reason"])

    def test_one_run_is_not_enough_to_call_anything(self):
        self.assertEqual(self.runs(COMPLETED)["class"], "unmeasured")

    def test_class_is_always_one_of_the_declared_values(self):
        for texts in (
            (COMPLETED,),
            (COMPLETED, HIT_CAP),
            (BLOCKED, BLOCKED),
            ("", ""),
            (COMPLETED, COMPLETED),
        ):
            self.assertIn(
                self.runs(*texts)["class"], fixture_manifest.STABILITY_CLASSES
            )

    def test_strip_stability_removes_every_derived_field(self):
        man = fixture_manifest.load_manifest()
        if man is None:
            self.skipTest("manifest not generated")
        stripped = fixture_manifest._strip_stability(man)
        self.assertNotIn("java_stability_source", stripped)
        for key in ("by_stability_class", "comparative_gate_eligible",
                    "java_finished_not_stable", "stability_measured"):
            self.assertNotIn(key, stripped["coverage"])
        for entry in stripped["fixtures"]:
            self.assertNotIn("stability", entry)
        # and the original is untouched
        self.assertIn("java_stability_source", man)


class TestRealCampaignLogs(unittest.TestCase):
    """Guards the specific claims measurement-campaign.md makes."""

    @classmethod
    def setUpClass(cls):
        if not os.path.isdir(campaign.UNCAPPED_DIR):
            raise unittest.SkipTest("uncapped results not present")
        cls.unc = fixture_manifest.load_uncapped(campaign.UNCAPPED_DIR)

    def test_all_fourteen_ex_timeout_boards_have_a_log(self):
        self.assertEqual(len(self.unc), 14)

    def test_every_completed_run_stayed_under_the_cap(self):
        for name, rec in self.unc.items():
            if rec["status"] == "completed":
                self.assertLess(rec["uncapped_wall_s"],
                                fixture_manifest.UNCAPPED_CAP_S, name)
                self.assertIsNotNone(rec["uncapped_unrouted"], name)

    def test_capped_runs_carry_no_citable_result(self):
        for name, rec in self.unc.items():
            if rec["hit_cap"]:
                self.assertGreaterEqual(rec["uncapped_wall_s"],
                                        fixture_manifest.UNCAPPED_CAP_S, name)

    def test_the_two_blocked_boards_are_the_rules_sidecar_pair(self):
        blocked = sorted(
            n for n, r in self.unc.items() if r["status"] == "blocked"
        )
        self.assertEqual(blocked, [
            "Issue029-hw48na.dsn",
            "Issue107-freq_teiler_200kHz_kicad_bad.dsn",
        ])
        for name in blocked:
            self.assertTrue(self.unc[name]["rules_sidecar"], name)

    def test_no_uncapped_run_executed_a_fanout_stage(self):
        # The whole campaign ran fanout-less; measurement-campaign.md §5 turns
        # on this, so it must fail loudly if a re-run changes it.
        for name, rec in self.unc.items():
            self.assertNotIn("fanout", rec["stages"], name)


class TestCheckWithoutLogs(unittest.TestCase):
    """`bench/results/**/*.log` is gitignored; --check must survive that."""

    def test_missing_default_uncapped_dir_is_not_an_error(self):
        gone = os.path.join(HERE, "no-such-uncapped-dir")
        saved = fixture_manifest.DEFAULT_UNCAPPED_DIR
        fixture_manifest.DEFAULT_UNCAPPED_DIR = gone
        try:
            rc = fixture_manifest.main(["--check", "--uncapped-dir", gone])
        finally:
            fixture_manifest.DEFAULT_UNCAPPED_DIR = saved
        self.assertEqual(rc, 0)

    def test_missing_explicit_uncapped_dir_is_an_error(self):
        gone = os.path.join(HERE, "no-such-uncapped-dir")
        rc = fixture_manifest.main(["--check", "--uncapped-dir", gone])
        self.assertEqual(rc, 2)

    def test_strip_uncapped_removes_every_derived_field(self):
        man = fixture_manifest.load_manifest()
        if man is None:
            self.skipTest("manifest not generated")
        stripped = fixture_manifest._strip_uncapped(man)
        self.assertNotIn("java_uncapped_source", stripped)
        self.assertNotIn("uncapped_measured", stripped["coverage"])
        self.assertNotIn("by_uncapped_status", stripped["coverage"])
        for entry in stripped["fixtures"]:
            self.assertNotIn("uncapped", entry)
        # and the original is untouched
        self.assertIn("java_uncapped_source", man)


class TestSections(unittest.TestCase):
    def test_every_section_renders(self):
        for d in (campaign.UNCAPPED_DIR, campaign.EXPERIMENTS_DIR,
                  campaign.BASELINE_DIR):
            if not os.path.isdir(d):
                self.skipTest(f"{d} not present")
        for name, fn in campaign.SECTIONS.items():
            text = fn()
            self.assertTrue(text.strip(), name)
            self.assertIn("|", text, name)


if __name__ == "__main__":
    unittest.main(verbosity=2)
