#!/usr/bin/env python3
"""Self-tests for the bench harness (stdlib unittest, no fixtures needed).

Covers the parts that decide what gets published: the per-net checker
breakdown, the Java classification rules, the dedup collapse, and the
quality-filtered geomean discipline.

    python3 bench/test_bench.py            # or: python3 -m unittest discover bench
"""

from __future__ import annotations

import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import check_ses  # noqa: E402
import manifest as fixture_manifest  # noqa: E402
import run_bench  # noqa: E402

DSN = """(pcb "t.dsn"
  (parser
    (string_quote ")
    (host_cad "KiCad's Pcbnew")
    (host_version "7.0.1")
  )
  (resolution um 10)
  (structure
    (layer F.Cu (type signal) (property (index 0)))
    (layer In1.Cu (type power) (property (index 1)))
    (snap_angle
      ninety
    )
    (autoroute_settings (fanout on) (vias on))
  )
  (placement
    (component "R:R_0603" (place R1 0 0 front 0))
  )
  (network
    (net A (pins R1-1 R2-1))
    (net B (pins R1-2))
    (class_class (classes A B) (rule (clearance 200)))
  )
  (wiring
    (wire (path F.Cu 250  0 0  1000 0)(net A)(type protect))
    (via "V0" 10 10 (net A)(type protect))
  )
)
"""

SES = """(session "t.ses"
  (routes
    (resolution um 10)
    (network_out
      (net A
        (wire (path F.Cu 250  0 0  1000 0))
        (wire (path B.Cu 250  0 0  0 500))
        (via "V0" 500 0)
      )
      (net C
        (via "V0" 700 0)
      )
    )
  )
)
"""


def write(tmpdir, name, text):
    path = os.path.join(tmpdir, name)
    with open(path, "w", encoding="utf-8") as f:
        f.write(text)
    return path


def rec(tool, fixture, **kw):
    """A finished bench record with plausible defaults."""
    base = {
        "tool": tool,
        "fixture": fixture,
        "timed_out": False,
        "crashed": False,
        "wall_s": 10.0 if tool == "java" else 1.0,
        "tool_time_s": 8.0 if tool == "java" else 2.0,
        "unrouted": 0,
        "violations": 0,
        "max_rss_bytes": 1024,
        "check": {"ses_nets_routed": 10, "dsn_nets_multi_pin": 10},
        "java_class": "java_finished",
        "dedup_group": "md5:" + fixture,
        "dedup_primary": True,
    }
    base.update(kw)
    return base


class PerNetChecker(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dsn = write(self.tmp.name, "t.dsn", DSN)
        self.ses = write(self.tmp.name, "t.ses", SES)
        self.addCleanup(self.tmp.cleanup)

    def test_per_net_sums_match_totals(self):
        out = check_ses.check(self.dsn, self.ses)
        per_net = out["per_net"]
        self.assertAlmostEqual(
            sum(v["wire_length_mm"] for v in per_net.values()),
            out["wire_length_mm"], places=3,
        )
        self.assertEqual(sum(v["via_count"] for v in per_net.values()),
                         out["via_count"])

    def test_per_net_lengths_use_the_ses_resolution(self):
        # (resolution um 10) => 1 raw unit = 0.1 um = 1e-4 mm.
        per_net = check_ses.check(self.dsn, self.ses)["per_net"]
        self.assertAlmostEqual(per_net["A"]["wire_length_mm"], 0.15, places=6)
        self.assertEqual(per_net["A"]["wire_count"], 2)
        self.assertEqual(per_net["A"]["via_count"], 1)

    def test_declared_but_unrouted_net_is_present_with_zeroes(self):
        per_net = check_ses.check(self.dsn, self.ses)["per_net"]
        self.assertEqual(per_net["B"],
                         {"pins": 1, "wire_count": 0, "via_count": 0,
                          "wire_length_mm": 0.0, "wire_length_raw": 0.0})

    def test_net_in_ses_but_not_in_dsn_has_null_pins(self):
        per_net = check_ses.check(self.dsn, self.ses)["per_net"]
        self.assertIsNone(per_net["C"]["pins"])
        self.assertEqual(per_net["C"]["via_count"], 1)

    def test_totals_only_drops_the_map(self):
        self.assertNotIn("per_net", check_ses.check(self.dsn, self.ses,
                                                    per_net=False))


class Properties(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dsn = write(self.tmp.name, "t.dsn", DSN)
        self.addCleanup(self.tmp.cleanup)

    def test_coverage_properties(self):
        p = fixture_manifest.parse_properties(self.dsn)
        self.assertTrue(p["is_specctra_dsn"])
        self.assertEqual((p["signal_layers"], p["plane_layers"]), (1, 1))
        self.assertEqual(p["snap_angle_declared"], "ninety")
        self.assertEqual(p["snap_angle_effective"], "ninety")
        self.assertEqual(p["host_cad"], "KiCad's Pcbnew")
        self.assertTrue(p["has_autoroute_settings"])
        self.assertTrue(p["has_class_class"])
        self.assertEqual((p["preexisting_wires"], p["preexisting_vias"]), (1, 1))
        self.assertTrue(p["has_preexisting_wiring"])
        self.assertEqual((p["nets_total"], p["nets_multi_pin"]), (2, 1))
        self.assertEqual(p["components_placed"], 1)

    def test_snap_angle_defaults_to_fortyfive_when_undeclared(self):
        bare = write(self.tmp.name, "b.dsn",
                     "(pcb \"b\" (structure (layer F.Cu (type signal))))")
        p = fixture_manifest.parse_properties(bare)
        self.assertIsNone(p["snap_angle_declared"])
        self.assertEqual(p["snap_angle_effective"], "fortyfive_degree")

    def test_non_dsn_input_is_flagged_not_crashed(self):
        junk = write(self.tmp.name, "junk.dsn", "\x00\x01binary garbage\xff")
        p = fixture_manifest.parse_properties(junk)
        self.assertFalse(p["is_specctra_dsn"])
        self.assertIsNotNone(p["parse_note"])

    def test_vias_without_wires_is_copper_but_not_wiring(self):
        # Issue145 ships pre-placed vias and no traces.
        d = write(self.tmp.name, "v.dsn",
                  '(pcb "v" (structure (layer F.Cu (type signal)))'
                  ' (wiring (via "V0" 1 1 (net A))))')
        p = fixture_manifest.parse_properties(d)
        self.assertFalse(p["has_preexisting_wiring"])
        self.assertTrue(p["has_preexisting_copper"])


class Classification(unittest.TestCase):
    props = {"nets_multi_pin": 10}

    def test_timeout(self):
        out = fixture_manifest.classify_java(
            rec("java", "f.dsn", timed_out=True, timeout_s=300.0), self.props)
        self.assertEqual(out["class"], "java_timeout")

    def test_crash(self):
        out = fixture_manifest.classify_java(
            rec("java", "f.dsn", crashed=True, exit_code=1), self.props)
        self.assertEqual(out["class"], "java_crash")

    def test_degenerate_zero_score(self):
        out = fixture_manifest.classify_java(
            rec("java", "f.dsn", score=0.0), self.props)
        self.assertEqual(out["class"], "java_degenerate")

    def test_degenerate_no_multi_pin_nets(self):
        out = fixture_manifest.classify_java(
            rec("java", "f.dsn", score=999.99), {"nets_multi_pin": 0})
        self.assertEqual(out["class"], "java_degenerate")

    def test_finished(self):
        out = fixture_manifest.classify_java(
            rec("java", "f.dsn", score=999.99), self.props)
        self.assertEqual(out["class"], "java_finished")
        self.assertIsNone(out["reason"])

    def test_missing_record_is_unknown(self):
        self.assertEqual(
            fixture_manifest.classify_java(None, self.props)["class"],
            "java_unknown")


class QualityFilter(unittest.TestCase):
    def judge(self, records, index=None):
        return run_bench.judge(run_bench.group_by_fixture(records),
                               index or {}, "placeholder-nets-v0")

    def test_passes_when_we_route_at_least_as_many_nets(self):
        v = self.judge([rec("java", "a.dsn"), rec("rust", "a.dsn")])
        self.assertTrue(v["a.dsn"]["ok"])

    def test_rejects_when_we_route_fewer_nets(self):
        v = self.judge([
            rec("java", "a.dsn"),
            rec("rust", "a.dsn",
                check={"ses_nets_routed": 3, "dsn_nets_multi_pin": 10}),
        ])
        self.assertFalse(v["a.dsn"]["ok"])
        self.assertIn("3 < java 10", v["a.dsn"]["reason"])

    def test_rejects_non_finished_java_class(self):
        for cls in ("java_timeout", "java_crash", "java_degenerate",
                    "java_unknown"):
            v = self.judge([rec("java", "a.dsn", java_class=cls),
                            rec("rust", "a.dsn", java_class=cls)])
            self.assertFalse(v["a.dsn"]["ok"], cls)
            self.assertEqual(v["a.dsn"]["reason"], cls)

    def test_rejects_when_rust_timed_out(self):
        v = self.judge([rec("java", "a.dsn"),
                        rec("rust", "a.dsn", timed_out=True)])
        self.assertFalse(v["a.dsn"]["ok"])

    def test_byte_identical_duplicate_counted_once(self):
        records = [
            rec("java", "a.dsn", dedup_group="md5:dup"),
            rec("rust", "a.dsn", dedup_group="md5:dup"),
            rec("java", "b.dsn", dedup_group="md5:dup", dedup_primary=False),
            rec("rust", "b.dsn", dedup_group="md5:dup", dedup_primary=False),
        ]
        v = self.judge(records)
        self.assertTrue(v["a.dsn"]["ok"])
        self.assertFalse(v["b.dsn"]["ok"])
        self.assertTrue(v["b.dsn"]["dedup_skip"])
        self.assertIn("duplicate of a.dsn", v["b.dsn"]["reason"])

    def test_lone_duplicate_member_still_counts(self):
        # A subset run containing only the non-primary member must not drop it.
        v = self.judge([rec("java", "b.dsn", dedup_group="md5:dup",
                            dedup_primary=False),
                        rec("rust", "b.dsn", dedup_group="md5:dup",
                            dedup_primary=False)])
        self.assertTrue(v["b.dsn"]["ok"])

    def test_fr_check_predicate_passes_nothing(self):
        v = run_bench.judge(
            run_bench.group_by_fixture([rec("java", "a.dsn"),
                                        rec("rust", "a.dsn")]),
            {}, "fr-check")
        self.assertFalse(v["a.dsn"]["ok"])


class GeomeanDiscipline(unittest.TestCase):
    """The published number may only come from quality-passing fixtures."""

    good = [rec("java", "a.dsn"), rec("rust", "a.dsn")]
    bad = [
        rec("java", "b.dsn", wall_s=100.0, tool_time_s=100.0),
        rec("rust", "b.dsn", wall_s=1.0, tool_time_s=1.0,
            check={"ses_nets_routed": 1, "dsn_nets_multi_pin": 10}),
    ]

    def render(self, records, **kw):
        return run_bench.render_report(records, {}, {}, "placeholder-nets-v0",
                                       **kw)

    def summary(self, records, **kw):
        return self.render(records, **kw).split("## Summary", 1)[1]

    def test_geomean_excludes_failing_fixtures(self):
        report = self.render(self.good + self.bad)
        # a.dsn is 10x; b.dsn would be 100x but routes 1/10 nets. The per-row
        # ratio is still shown (that is data); only the aggregate must drop it.
        self.assertIn("| 100.0x |", report)
        summary = report.split("## Summary", 1)[1]
        self.assertIn("quality-filtered**: wall-clock 10.0x (n=1)", summary)
        self.assertNotIn("100.0x", summary)

    def test_unfiltered_geomean_absent_unless_requested(self):
        self.assertNotIn("quality-unfiltered",
                         self.render(self.good + self.bad))

    def test_unfiltered_geomean_is_tagged_not_comparable(self):
        report = self.render(self.good + self.bad, emit_unfiltered=True)
        self.assertIn("quality-unfiltered, not comparable", report)
        self.assertIn("31.6x", report)  # geomean(10, 100)

    def test_refuses_a_number_when_nothing_passes(self):
        report = self.render(self.bad)
        self.assertIn("n/a — 0 fixtures passed", report)
        self.assertIn("No speed claim may be made", report)

    def test_missing_manifest_classifies_everything_unknown(self):
        anon = [dict(r, java_class=None) for r in self.good]
        report = run_bench.render_report(anon, {}, {}, "placeholder-nets-v0")
        self.assertIn("java_unknown 1", report)
        self.assertIn("n/a — 0 fixtures passed", report)


class ResultsIO(unittest.TestCase):
    def test_load_results_accepts_both_shapes(self):
        import json
        with tempfile.TemporaryDirectory() as tmp:
            legacy = write(tmp, "old.json", json.dumps([rec("java", "a.dsn")]))
            meta, records = fixture_manifest.load_results(legacy)
            self.assertEqual(meta, {})
            self.assertEqual(len(records), 1)

            envelope = write(tmp, "new.json", json.dumps(
                {"meta": {"config_label": "mp9999"},
                 "records": [rec("java", "a.dsn")]}))
            meta, records = fixture_manifest.load_results(envelope)
            self.assertEqual(meta["config_label"], "mp9999")
            self.assertEqual(len(records), 1)


class TokenizerQuoteHandling(unittest.TestCase):
    """Regression tests for the unescaped-quote desync (adversarial review).

    KiCad writes inch marks unescaped inside quoted strings, and Eagle writes
    pin references as `"J3"-"D+"`. A tokenizer that ends a string at the very
    next `"` desyncs on both: it used to swallow whole records (Issue229 lost
    4 of its 26 `place` nodes) and to split one pin reference into three
    atoms (inflating Issue143's multi-pin net count from the true 4 to 5).
    """

    def test_unescaped_inch_mark_does_not_swallow_the_following_record(self):
        text = (
            '(placement'
            '  (component X'
            '    (place DP2 1 2 front 0 (PN "DISPLAY 7-SEG 0.5""))'
            '    (place DP3 3 4 front 0 (PN "DISPLAY 7-SEG 0.5""))'
            '    (place DP4 5 6 front 0 (PN "PLAIN"))'
            '  )'
            ')'
        )
        tree = check_ses.parse_sexp(text)
        names = [n[1] for n in check_ses.iter_nodes(tree, "place")]
        self.assertEqual(names, ["DP2", "DP3", "DP4"])

    def test_quoted_pin_reference_stays_one_atom(self):
        text = '(network (net "VBUS" (pins "J3"-"VBUS")) (net "D+" (pins "J3"-"D+" "J1"-"D+")))'
        tree = check_ses.parse_sexp(text)
        counts = {}
        for net in check_ses.iter_nodes(tree, "net"):
            pins = 0
            for node in check_ses.child_nodes(net, "pins"):
                pins += sum(1 for el in node[1:] if isinstance(el, str))
            counts[net[1]] = pins
        # A one-pin net must not look multi-pin just because its pin
        # reference contains quotes and a dash.
        self.assertEqual(counts, {"VBUS": 1, "D+": 2})

    def test_ordinary_quoted_strings_are_unaffected(self):
        tree = check_ses.parse_sexp(
            '(pcb (parser (host_cad "KiCad\'s Pcbnew") (host_version "8.0")))'
        )
        got = [n[1] for n in check_ses.iter_nodes(tree, "host_cad")]
        self.assertEqual(got, ["KiCad's Pcbnew"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
