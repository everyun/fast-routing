#!/usr/bin/env python3
"""Self-tests for `settings_scoreboard.py`. Hermetic parsers plus a live run
against the 2.3.0 source when `/tmp/fr-java-release/src` is present."""

from __future__ import annotations

import os
import sys
import unittest
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import settings_scoreboard as ss  # noqa: E402

JAVA_SNIPPET = """
public class RouterSettings {
  @SerializedName("enabled")
  public Boolean enabled;
  @SerializedName("ignore_net_classes")
  public transient String[] ignoreNetClasses;
  @SerializedName("max_passes")
  public Integer maxPasses;
  public transient Boolean save_intermediate_stages = false;
  private transient Boolean boardSpecificTraceCostsApplied;
}
"""

RUST_SNIPPET = """
pub struct RouterSettings {
    pub enabled: Option<bool>,
    pub max_passes: i32,
    pub ignore_net_classes: Vec<String>,
    pub vias_allowed: bool,
}

impl RouterSettings {
    pub fn run_router(&self) -> bool { true }
}
"""


class ParseTests(unittest.TestCase):
    def test_java_serialized_name_and_transient(self):
        fields = ss.parse_java_class(JAVA_SNIPPET)
        keys = {json_key for json_key, _field, _ty in fields}
        self.assertIn("enabled", keys)
        self.assertIn("ignore_net_classes", keys)
        self.assertIn("max_passes", keys)
        self.assertIn("save_intermediate_stages", keys)
        self.assertNotIn("boardSpecificTraceCostsApplied", keys)
        self.assertNotIn("board_specific_trace_costs_applied", keys)

    def test_rust_struct_fields(self):
        structs = ss.parse_rust_structs(RUST_SNIPPET)
        self.assertIn("enabled", structs["RouterSettings"])
        self.assertIn("ignore_net_classes", structs["RouterSettings"])

    def test_camel_to_snake(self):
        self.assertEqual(ss.camel_to_snake("ignoreNetClasses"), "ignore_net_classes")
        self.assertEqual(ss.camel_to_snake("maxPasses"), "max_passes")


@unittest.skipUnless(ss.find_java_settings_dir() is not None, "Java source not mounted")
class LiveScoreboardTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.knobs = {k.key: k for k in ss.collect()}

    def test_ignore_net_classes_is_intentionally_dead(self):
        k = self.knobs["router.ignore_net_classes"]
        self.assertEqual(k.status, "intentionally-dead")
        self.assertIn("PARITY", k.evidence)

    def test_dsn_fanout_is_intentionally_dead(self):
        self.assertEqual(self.knobs["dsn.fanout"].status, "intentionally-dead")

    def test_dsn_autoroute_is_implemented(self):
        self.assertEqual(self.knobs["dsn.autoroute"].status, "implemented")

    def test_max_passes_is_implemented(self):
        self.assertEqual(self.knobs["router.max_passes"].status, "implemented")

    def test_cli_mp_is_implemented(self):
        self.assertEqual(self.knobs["cli.-mp"].status, "implemented")

    def test_cli_us_is_rejected(self):
        self.assertEqual(self.knobs["cli.-us"].status, "rejected")

    def test_generic_router_setter_is_rejected(self):
        self.assertEqual(self.knobs["cli.--router.*"].status, "rejected")

    def test_notes_come_from_the_dict(self):
        k = self.knobs["router.ignore_net_classes"]
        self.assertEqual(k.note, ss.NOTES["router.ignore_net_classes"])

    def test_render_is_deterministic(self):
        java_dir = ss.find_java_settings_dir()
        knobs = ss.collect(java_dir)
        a = ss.render(knobs, java_dir)
        b = ss.render(knobs, java_dir)
        self.assertEqual(a, b)
        self.assertIn("| `router.ignore_net_classes` | intentionally-dead |", a)

    def test_every_java_knob_has_a_status(self):
        for key, k in self.knobs.items():
            self.assertIn(
                k.status,
                {"implemented", "intentionally-dead", "rejected", "not-yet"},
                msg=key,
            )

    def test_no_method_locals_in_the_universe(self):
        for junk in (
            "router.true",
            "router.false",
            "router.return",
            "router.old_value",
            "router.available_processors",
            "router.layers[].copy",
            "router.algorithm_current",
            "router.min_bend_cost",
        ):
            self.assertNotIn(junk, self.knobs)

    def test_job_timeout_string_is_not_the_cli_flag(self):
        self.assertEqual(self.knobs["router.job_timeout"].status, "not-yet")


if __name__ == "__main__":
    unittest.main()
