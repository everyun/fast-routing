#!/usr/bin/env python3
"""Hermetic tests for `bench/tools/jar_audit.py`.

A well-formed fake jar (the four required classes + a 2d4de019 Build-Revision)
must pass. Deleting any required class, injecting a forbidden package, or
changing the revision must fail. No real 62 MB jar in the loop.

Run: `python3 -m unittest discover -s bench/tools/tests`
"""

from __future__ import annotations

import io
import os
import sys
import unittest
import zipfile

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import jar_audit  # noqa: E402

REVISION = "2d4de019aa89e9fa3dc1dc44e09bf509760cafc1"
CAFEBABE = b"\xca\xfe\xba\xbe" + b"\x00" * 16


def manifest(revision: str = REVISION) -> bytes:
    return (
        "Manifest-Version: 1.0\n"
        "Build-Date: 2026-08-07\n"
        f"Build-Revision: {revision}\n"
        "Main-Class: app.freerouting.Freerouting\n"
    ).encode("utf-8")


def build_jar(classes: dict[str, bytes] | None = None, revision: str = REVISION) -> bytes:
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as zf:
        zf.writestr("META-INF/MANIFEST.MF", manifest(revision))
        payload = classes if classes is not None else {
            name: CAFEBABE for name in jar_audit.REQUIRED_CLASSES
        }
        for name, data in payload.items():
            zf.writestr(name, data)
    return buf.getvalue()


def audit_bytes(raw: bytes, revision: str | None = None) -> dict:
    buf = io.BytesIO(raw)
    with zipfile.ZipFile(buf) as zf:
        return jar_audit.audit_zip(zf, revision=revision or REVISION)


class WellFormedJar(unittest.TestCase):
    def test_required_classes_and_revision_pass(self):
        result = audit_bytes(build_jar())
        self.assertTrue(result["ok"], result["failures"])
        self.assertEqual(result["build_revision"], REVISION)
        self.assertEqual(result["forbidden_hits"], [])

    def test_packages_lists_autoroute_and_interactive(self):
        result = audit_bytes(build_jar())
        self.assertIn("app/freerouting/autoroute/", result["packages"])
        self.assertIn("app/freerouting/interactive/", result["packages"])


class TamperedJar(unittest.TestCase):
    def test_missing_maze_search_algo_fails(self):
        classes = {n: CAFEBABE for n in jar_audit.REQUIRED_CLASSES}
        del classes["app/freerouting/autoroute/MazeSearchAlgo.class"]
        result = audit_bytes(build_jar(classes=classes))
        self.assertFalse(result["ok"])
        blob = " ".join(result["failures"])
        self.assertIn("MazeSearchAlgo", blob)

    def test_missing_each_required_class_fails(self):
        for missing in jar_audit.REQUIRED_CLASSES:
            classes = {n: CAFEBABE for n in jar_audit.REQUIRED_CLASSES if n != missing}
            result = audit_bytes(build_jar(classes=classes))
            self.assertFalse(result["ok"], missing)
            self.assertTrue(
                any(os.path.basename(missing) in f for f in result["failures"]),
                result["failures"],
            )

    def test_pipeline_package_is_forbidden(self):
        classes = {n: CAFEBABE for n in jar_audit.REQUIRED_CLASSES}
        classes["app/freerouting/autoroute/pipeline/BatchAutorouter.class"] = CAFEBABE
        result = audit_bytes(build_jar(classes=classes))
        self.assertFalse(result["ok"])
        self.assertTrue(any("pipeline" in h for h in result["forbidden_hits"]))

    def test_maze_package_is_forbidden(self):
        classes = {n: CAFEBABE for n in jar_audit.REQUIRED_CLASSES}
        classes["app/freerouting/autoroute/maze/MazeSearchEngine.class"] = CAFEBABE
        result = audit_bytes(build_jar(classes=classes))
        self.assertFalse(result["ok"])

    def test_facade_package_is_forbidden(self):
        classes = {n: CAFEBABE for n in jar_audit.REQUIRED_CLASSES}
        classes["app/freerouting/board/facade/RoutingBoard.class"] = CAFEBABE
        result = audit_bytes(build_jar(classes=classes))
        self.assertFalse(result["ok"])

    def test_wrong_build_revision_fails(self):
        result = audit_bytes(build_jar(revision="deadbeef00000000"))
        self.assertFalse(result["ok"])
        blob = " ".join(result["failures"])
        self.assertIn("Build-Revision", blob)

    def test_missing_manifest_fails(self):
        buf = io.BytesIO()
        with zipfile.ZipFile(buf, "w") as zf:
            for name in jar_audit.REQUIRED_CLASSES:
                zf.writestr(name, CAFEBABE)
        result = audit_bytes(buf.getvalue())
        self.assertFalse(result["ok"])


class PrefixPin(unittest.TestCase):
    def test_prefix_pin_accepts_full_hash(self):
        result = audit_bytes(build_jar(), revision="2d4de019")
        self.assertTrue(result["ok"], result["failures"])


if __name__ == "__main__":
    unittest.main()
