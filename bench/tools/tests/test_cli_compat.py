#!/usr/bin/env python3
"""Self-tests for `cli_compat.py`."""

from __future__ import annotations

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import cli_compat as cc  # noqa: E402

JAVA_SNIPPET = r"""
        if (p_args[i].equalsIgnoreCase("-help") || p_args[i].equalsIgnoreCase("--help") || p_args[i].equalsIgnoreCase("-h")) {
        }
        if (p_args[i].startsWith("--compare-boards=")) {
        }
        } else if (p_args[i].startsWith("-de")) {
        } else if (p_args[i].startsWith("-di")) {
        } else if (p_args[i].startsWith("-do")) {
        } else if (p_args[i].startsWith("-drc")) {
        } else if (p_args[i].startsWith("-mp")) {
        } else if (p_args[i].startsWith("-us")) {
        } else if (p_args[i].startsWith("-inc")) {
"""

OURS_SNIPPET = r"""
            "-de" | "--design" | "-di" => args.design = Some(next!("-de")),
            "-do" | "--output" => args.output = Some(next!("-do")),
            "-mp" | "--max-passes" => args.max_passes = Some(parse_max_passes(&next!("-mp"))),
            "-us" | "--threads" => args.threads = Some(parse_count("--threads", &next!("-us"))),
            "--portfolio" => { }
            "--no-optimizer" => args.no_optimizer = true,
            "-h" | "--help" => { }
"""


class ParseTests(unittest.TestCase):
    def test_java_starts_with_and_equals(self):
        names = set(cc.parse_java_flags(JAVA_SNIPPET))
        self.assertIn("-de", names)
        self.assertIn("-us", names)
        self.assertIn("-help", names)
        self.assertIn("--help", names)
        self.assertIn("-h", names)
        self.assertIn("--compare-boards", names)
        self.assertIn("--user_data_path", names)

    def test_ours_match_arms(self):
        names = set(cc.parse_ours_flags(OURS_SNIPPET))
        self.assertIn("-de", names)
        self.assertIn("--design", names)
        self.assertIn("-us", names)
        self.assertIn("--threads", names)
        self.assertIn("--portfolio", names)
        self.assertIn("--no-optimizer", names)


class ClassifyTests(unittest.TestCase):
    def test_us_collision_is_unsupported(self):
        flag = cc.classify_pair("-us", {"-us"}, {"-us", "--threads"})
        self.assertEqual(flag.status, "不支持")

    def test_threads_is_ours(self):
        flag = cc.classify_pair("--threads", {"-us", "-mt"}, {"--threads", "-us"})
        self.assertEqual(flag.status, "我方独有")

    def test_do_is_compatible(self):
        flag = cc.classify_pair("-do", {"-do"}, {"-do"})
        self.assertEqual(flag.status, "兼容")

    def test_mp_is_partial(self):
        flag = cc.classify_pair("-mp", {"-mp"}, {"-mp"})
        self.assertEqual(flag.status, "部分")

    def test_portfolio_is_ours(self):
        flag = cc.classify_pair("--portfolio", set(), {"--portfolio"})
        self.assertEqual(flag.status, "我方独有")

    def test_inc_is_unsupported(self):
        flag = cc.classify_pair("-inc", {"-inc"}, set())
        self.assertEqual(flag.status, "不支持")


@unittest.skipUnless(cc.find_global_settings() is not None, "Java source not mounted")
class LiveCliTests(unittest.TestCase):
    def test_table_contains_required_rows(self):
        flags = {f.name: f for f in cc.collect()}
        self.assertEqual(flags["-us"].status, "不支持")
        self.assertEqual(flags["--threads"].status, "我方独有")
        self.assertEqual(flags["--portfolio"].status, "我方独有")
        self.assertEqual(flags["--no-optimizer"].status, "我方独有")
        self.assertEqual(flags["-do"].status, "兼容")
        self.assertEqual(flags["-mp"].status, "部分")
        text = cc.render(list(flags.values()))
        self.assertIn("`-us` collision", text)
        self.assertEqual(text, cc.render(list(flags.values())))


if __name__ == "__main__":
    unittest.main()
