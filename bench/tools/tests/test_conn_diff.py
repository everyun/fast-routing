#!/usr/bin/env python3
"""Unit tests for bench/tools/conn_diff.py (keyed matching and out-of-order logs)."""

import tempfile
import unittest
from pathlib import Path

from bench.tools.conn_diff import Conn, diff_pass_keyed, diff_pass_positional, parse


class TestConnDiffKeyed(unittest.TestCase):
    def test_out_of_order_matching(self):
        """Tests that keyed diff correctly matches identical connections regardless of order."""
        # Conn A: net 2 at Pin@100,200
        # Conn B: net 60 at Pin@300,400
        # Conn C: net 42 at Pin@500,600
        conn_a_ours = Conn(
            pass_no=1, net=2, src="Pin@100,200", exp=113, ok="ROUTED",
            traces=1, path="100,200;150,200", vias="0:", raw=""
        )
        conn_b_ours = Conn(
            pass_no=1, net=60, src="Pin@300,400", exp=2224, ok="ROUTED",
            traces=1, path="300,400;350,400", vias="0:", raw=""
        )
        conn_c_ours = Conn(
            pass_no=1, net=42, src="Pin@500,600", exp=50, ok="ROUTED",
            traces=1, path="500,600;550,600", vias="0:", raw=""
        )

        # Java has them in completely reversed/different order with different expansions
        conn_b_java = Conn(
            pass_no=1, net=60, src="Pin@300,400", exp=1168, ok="ROUTED",
            traces=1, path="300,400;350,400", vias="0:", raw=""
        )
        conn_c_java = Conn(
            pass_no=1, net=42, src="Pin@500,600", exp=45, ok="ROUTED",
            traces=1, path="500,600;550,600", vias="0:", raw=""
        )
        conn_a_java = Conn(
            pass_no=1, net=2, src="Pin@100,200", exp=337, ok="ROUTED",
            traces=1, path="100,200;150,200", vias="0:", raw=""
        )

        ours = [conn_a_ours, conn_b_ours, conn_c_ours]
        java = [conn_b_java, conn_c_java, conn_a_java]

        # Under positional comparison, they fail at index 0 because conn_a != conn_b
        pos_res = diff_pass_positional(ours, java, context=1)
        self.assertEqual(pos_res["first_divergence"], 0)
        self.assertFalse(pos_res["aligned"])

        # Under keyed comparison, all 3 are correctly matched by (net, src)
        keyed_res = diff_pass_keyed(ours, java)
        self.assertEqual(keyed_res["matched_count"], 3)
        self.assertEqual(keyed_res["ours_only_count"], 0)
        self.assertEqual(keyed_res["java_only_count"], 0)
        self.assertEqual(keyed_res["status_mismatch_count"], 0)

    def test_single_sided_reporting(self):
        """Tests that connections present only in ours or only in java are captured."""
        conn_ours_only = Conn(
            pass_no=1, net=99, src="Pin@999,999", exp=10, ok="ROUTED",
            traces=1, path="999,999;1000,1000", vias="0:", raw=""
        )
        conn_common_ours = Conn(
            pass_no=1, net=1, src="Pin@10,10", exp=100, ok="ROUTED",
            traces=1, path="10,10;20,20", vias="0:", raw=""
        )
        conn_common_java = Conn(
            pass_no=1, net=1, src="Pin@10,10", exp=105, ok="ROUTED",
            traces=1, path="10,10;20,20", vias="0:", raw=""
        )
        conn_java_only = Conn(
            pass_no=1, net=88, src="Pin@888,888", exp=20, ok="FAILED",
            traces=0, path="", vias="0:", raw=""
        )

        ours = [conn_ours_only, conn_common_ours]
        java = [conn_common_java, conn_java_only]

        keyed_res = diff_pass_keyed(ours, java)
        self.assertEqual(keyed_res["matched_count"], 1)
        self.assertEqual(keyed_res["ours_only_count"], 1)
        self.assertEqual(keyed_res["java_only_count"], 1)
        self.assertIn("net=99 src=Pin@999,999", keyed_res["ours_only"][0])
        self.assertIn("net=88 src=Pin@888,888", keyed_res["java_only"][0])

    def test_status_mismatch_detection(self):
        """Tests detection when one side ROUTED and the other side FAILED on the same pin."""
        conn_ours = Conn(
            pass_no=1, net=5, src="Pin@12,34", exp=50000, ok="FAILED",
            traces=0, path="", vias="0:", raw=""
        )
        conn_java = Conn(
            pass_no=1, net=5, src="Pin@12,34", exp=200, ok="ROUTED",
            traces=1, path="12,34;56,78", vias="0:", raw=""
        )

        keyed_res = diff_pass_keyed([conn_ours], [conn_java])
        self.assertEqual(keyed_res["matched_count"], 1)
        self.assertEqual(keyed_res["status_mismatch_count"], 1)
        self.assertEqual(keyed_res["status_mismatches"][0]["key"], "net=5 src=Pin@12,34")

    def test_file_parsing(self):
        """Tests parsing real log lines from temporary files."""
        ours_log = (
            "FRCONN conn pass=1 net=2 src=Pin@837226,-1253938 item=333 exp=113 ok=ROUTED traces=1 path=837,125;831,125 vias=0:\n"
            "FRCONN conn pass=1 net=62 src=Pin@1227184,-749682 item=62 exp=109634 ok=FAILED traces=0 path= vias=0:\n"
        )
        java_log = (
            "2026-09-06 11:09:32.454 INFO FRLAB conn pass=1 net=62 src=Pin@1227184,-749682 item=62 exp=87775 ok=FAILED traces=0 path= vias=0:\n"
            "2026-09-06 11:09:30.832 INFO FRLAB conn pass=1 net=2 src=Pin@837226,-1253938 item=333 exp=337 ok=ROUTED traces=1 path=837,125;831,125 vias=0:\n"
        )

        with tempfile.NamedTemporaryFile("w+", delete=False) as f_ours, \
             tempfile.NamedTemporaryFile("w+", delete=False) as f_java:
            f_ours.write(ours_log)
            f_ours.flush()
            f_java.write(java_log)
            f_java.flush()

            ours = parse(f_ours.name)
            java = parse(f_java.name)

            diff = diff_pass_keyed(ours[1], java[1])
            self.assertEqual(diff["matched_count"], 2)
            self.assertEqual(diff["status_mismatch_count"], 0)
            self.assertEqual(diff["ours_only_count"], 0)
            self.assertEqual(diff["java_only_count"], 0)


if __name__ == "__main__":
    unittest.main()
