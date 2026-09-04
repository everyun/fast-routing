#!/usr/bin/env python3
"""Audit the Freerouting reference jar against docs/ARCHITECTURE.md "The reference".

The released 2.3.0 artifact is a hybrid: v19 router-core class names in the
flat `autoroute/` package, v2 IO/DRC. CI must fail if that mix silently
drifts (git-master `pipeline` / `maze` / `facade` packages appearing, or
the v19 core classes disappearing).

Stdlib only. Exit 0 on a clean audit, 1 on any failed assertion.

    python3 bench/tools/jar_audit.py /tmp/frbase/freerouting-2.3.0.jar
    python3 bench/tools/jar_audit.py --json JAR
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import zipfile

REQUIRED_CLASSES = (
    "app/freerouting/autoroute/MazeSearchAlgo.class",
    "app/freerouting/autoroute/AutorouteEngine.class",
    "app/freerouting/autoroute/BatchFanout.class",
    "app/freerouting/interactive/AutorouterAndRouteOptimizerThread.class",
)

# Package prefixes that exist on git master and MUST NOT exist in 2.3.0.
FORBIDDEN_PACKAGES = (
    "app/freerouting/autoroute/pipeline/",
    "app/freerouting/autoroute/maze/",
    "app/freerouting/board/facade/",
)

# docs/ARCHITECTURE.md: Build-Revision `2d4de019aa89e9fa3dc1dc44e09bf509760cafc1`.
# The task asks for equality with the `2d4de019…` prefix; we accept that prefix
# and, when pins.json is present, the full hash.
DEFAULT_REVISION_PREFIX = "2d4de019"

PINS_PATH = os.path.join(
    os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))),
    "ci",
    "pins.json",
)


def load_pins(path: str | None = None) -> dict:
    pins_path = path or PINS_PATH
    if os.path.isfile(pins_path):
        with open(pins_path, encoding="utf-8") as f:
            return json.load(f)
    return {}


def expected_revision(pins: dict | None = None) -> str:
    pins = pins if pins is not None else load_pins()
    rev = (pins.get("jar") or {}).get("build_revision") or DEFAULT_REVISION_PREFIX
    return str(rev)


def parse_manifest(raw: bytes) -> dict[str, str]:
    text = raw.decode("utf-8", "replace")
    # Continuation lines start with a space (JAR spec).
    lines: list[str] = []
    for line in text.splitlines():
        if line.startswith(" ") and lines:
            lines[-1] += line[1:]
        else:
            lines.append(line)
    out: dict[str, str] = {}
    for line in lines:
        if ":" not in line:
            continue
        key, _, value = line.partition(":")
        out[key.strip()] = value.strip()
    return out


def class_entries(names: list[str]) -> list[str]:
    return [n for n in names if n.endswith(".class") and not n.endswith("/")]


def packages_of(class_names: list[str]) -> list[str]:
    pkgs: set[str] = set()
    for name in class_names:
        parent = name.rsplit("/", 1)[0] if "/" in name else ""
        if parent:
            pkgs.add(parent + "/")
    return sorted(pkgs)


def audit_zip(zf: zipfile.ZipFile, *, revision: str | None = None) -> dict:
    names = zf.namelist()
    classes = class_entries(names)
    class_set = set(classes)
    pkgs = packages_of(classes)

    missing = [c for c in REQUIRED_CLASSES if c not in class_set]
    forbidden_hits = [
        n
        for n in names
        if any(n.startswith(prefix) for prefix in FORBIDDEN_PACKAGES)
    ]

    manifest_entry = None
    for candidate in ("META-INF/MANIFEST.MF", "meta-inf/manifest.mf"):
        if candidate in names:
            manifest_entry = candidate
            break
        # zip namelist is case-sensitive; search case-insensitively too
    if manifest_entry is None:
        for n in names:
            if n.upper() == "META-INF/MANIFEST.MF":
                manifest_entry = n
                break

    manifest: dict[str, str] = {}
    build_revision = None
    if manifest_entry is not None:
        manifest = parse_manifest(zf.read(manifest_entry))
        build_revision = manifest.get("Build-Revision")

    want = revision or expected_revision()
    revision_ok = bool(
        build_revision and build_revision.startswith(want[:8] if len(want) >= 8 else want)
    )
    # Prefer full-hash equality when the pin is the full git sha.
    if build_revision and len(want) >= 40:
        revision_ok = build_revision == want

    failures: list[str] = []
    if missing:
        failures.append("missing required class(es): " + ", ".join(missing))
    if forbidden_hits:
        failures.append(
            "forbidden package entries present: " + ", ".join(sorted(set(forbidden_hits))[:20])
        )
    if manifest_entry is None:
        failures.append("META-INF/MANIFEST.MF missing")
    elif not build_revision:
        failures.append("Build-Revision missing from MANIFEST.MF")
    elif not revision_ok:
        failures.append(
            f"Build-Revision {build_revision!r} does not match pin {want!r}"
        )

    return {
        "ok": not failures,
        "failures": failures,
        "packages": pkgs,
        "class_count": len(classes),
        "required": {c: c in class_set for c in REQUIRED_CLASSES},
        "forbidden_hits": forbidden_hits,
        "build_revision": build_revision,
        "expected_revision": want,
        "manifest": {k: manifest[k] for k in ("Build-Revision", "Build-Date", "Main-Class") if k in manifest},
    }


def audit_path(path: str, *, revision: str | None = None) -> dict:
    if not os.path.isfile(path):
        return {
            "ok": False,
            "failures": [f"jar not found: {path}"],
            "packages": [],
            "class_count": 0,
            "required": {c: False for c in REQUIRED_CLASSES},
            "forbidden_hits": [],
            "build_revision": None,
            "expected_revision": revision or expected_revision(),
            "manifest": {},
        }
    with zipfile.ZipFile(path) as zf:
        result = audit_zip(zf, revision=revision)
    result["jar"] = path
    return result


def format_report(result: dict) -> str:
    lines = []
    jar = result.get("jar", "<zip>")
    lines.append(f"jar: {jar}")
    lines.append(f"classes: {result.get('class_count', 0)}")
    rev = result.get("build_revision")
    lines.append(f"Build-Revision: {rev}")
    lines.append("required classes:")
    for name, present in (result.get("required") or {}).items():
        lines.append(f"  {'OK' if present else 'MISSING'}  {name}")
    hits = result.get("forbidden_hits") or []
    lines.append(f"forbidden package hits: {len(hits)}")
    for hit in hits[:20]:
        lines.append(f"  {hit}")
    lines.append("packages (app/freerouting/*, truncated to router-relevant):")
    for pkg in result.get("packages") or []:
        if pkg.startswith("app/freerouting/"):
            lines.append(f"  {pkg}")
    if result.get("ok"):
        lines.append("AUDIT PASS")
    else:
        lines.append("AUDIT FAIL")
        for f in result.get("failures") or []:
            lines.append(f"  - {f}")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("jar", nargs="?", default=None, help="path to freerouting-*.jar")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--revision", default=None, help="override expected Build-Revision")
    ap.add_argument("--pins", default=None, help="override ci/pins.json")
    args = ap.parse_args(argv)

    pins = load_pins(args.pins) if args.pins or os.path.isfile(PINS_PATH) else {}
    jar = args.jar or (pins.get("jar") or {}).get("path") or "/tmp/frbase/freerouting-2.3.0.jar"
    revision = args.revision or expected_revision(pins)
    result = audit_path(jar, revision=revision)
    if args.json:
        print(json.dumps(result, indent=2, sort_keys=True))
    else:
        print(format_report(result))
    return 0 if result["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
