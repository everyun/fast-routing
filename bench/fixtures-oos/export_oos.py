#!/usr/bin/env python3
"""Build bench/fixtures-oos/: copy sources, export Specctra DSN, write manifest.

Must be run with KiCad's Python (re-execs itself). Never writes into the CLA45 vault.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
CATALOG = HERE / "catalog.json"
PCB_DIR = HERE / "pcb"
DSN_DIR = HERE
MANIFEST_OUT = HERE / "manifest.json"
SOURCES_OUT = HERE / "sources.json"

VAULT = (
    Path.home()
    / "Documents"
    / "Env Config Vault"
    / "10 资产台账"
    / "CLA45 车顶 HDR 相机"
)
DEMOS = HERE / "demos"
FR_JAVA = Path.home() / "fr-ref" / "fr-java-release"
KICAD_APP = Path.home() / "Applications" / "KiCad" / "KiCad.app"
KICAD_PY = (
    KICAD_APP
    / "Contents"
    / "Frameworks"
    / "Python.framework"
    / "Versions"
    / "3.9"
    / "bin"
    / "python3"
)
KICAD_CLI = KICAD_APP / "Contents" / "MacOS" / "kicad-cli"

# GitLab 10.0.5 demo PCBs we actually need.
DEMO_FETCH = {
    "kit-dev-coldfire-xilinx_5213/kit-dev-coldfire-xilinx_5213.kicad_pcb": (
        "https://gitlab.com/kicad/code/kicad/-/raw/10.0.5/demos/"
        "kit-dev-coldfire-xilinx_5213/kit-dev-coldfire-xilinx_5213.kicad_pcb"
    ),
}


def _reexec_kicad_python() -> None:
    if "pcbnew" in sys.modules:
        return
    if Path(sys.executable).resolve() == KICAD_PY.resolve():
        return
    os.execv(str(KICAD_PY), [str(KICAD_PY), str(Path(__file__).resolve()), *sys.argv[1:]])


def origin_path(board: dict) -> Path:
    root = board["origin_root"]
    rel = board["origin"]
    if root == "vault":
        return VAULT / rel
    if root == "demos":
        return DEMOS / rel
    if root == "fr-java-release":
        return FR_JAVA / rel
    raise ValueError(f"unknown origin_root {root}")


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def fetch_demos() -> None:
    """Download official KiCad demo PCBs. Uses curl (system certs); KiCad's
    bundled CPython 3.9 does not have a cert store."""
    import subprocess

    mirrors = [
        "https://gitlab.com/kicad/code/kicad/-/raw/10.0.5/demos/{rel}",
        "https://raw.githubusercontent.com/KiCad/kicad-source-mirror/10.0.5/demos/{rel}",
        "https://gitlab.com/kicad/code/kicad/-/raw/master/demos/{rel}",
    ]
    for rel, _preferred in DEMO_FETCH.items():
        dest = DEMOS / rel
        if dest.exists() and dest.stat().st_size > 1000:
            continue
        dest.parent.mkdir(parents=True, exist_ok=True)
        last_err = None
        for tmpl in mirrors:
            url = tmpl.format(rel=rel)
            print(f"fetch {url} -> {dest}")
            r = subprocess.run(
                ["curl", "-fsSL", "--max-time", "120", "-o", str(dest), url],
                capture_output=True, text=True,
            )
            if r.returncode == 0 and dest.exists() and dest.stat().st_size > 1000:
                print(f"  {dest.stat().st_size} bytes")
                last_err = None
                break
            last_err = r.stderr.strip() or f"curl exit {r.returncode}"
            dest.unlink(missing_ok=True)
        if last_err:
            raise RuntimeError(f"failed to fetch {rel}: {last_err}")


def patch_dsn_text(text: str, plane_patch: list[str], snap: str | None) -> str:
    for layer in plane_patch:
        # KiCad writes `(layer NAME\n      (type signal)` across lines.
        pattern = rf"(\(layer {re.escape(layer)}\s+\(type )signal(\))"
        text, n = re.subn(pattern, rf"\1power\2", text, count=1)
        if n != 1:
            pattern_q = rf'(\(layer "{re.escape(layer)}"\s+\(type )signal(\))'
            text, n = re.subn(pattern_q, rf"\1power\2", text, count=1)
        if n != 1:
            raise RuntimeError(f"plane_patch missed layer {layer}")
    if snap:
        if re.search(r"\(snap_angle\s+\S+\)", text):
            text = re.sub(r"\(snap_angle\s+\S+\)", f"(snap_angle {snap})", text, count=1)
        else:
            # Insert as a structure child. Specctra readers accept it next to layers.
            text, n = re.subn(
                r"(\(structure\b)",
                rf"\1\n    (snap_angle {snap})",
                text,
                count=1,
            )
            if n != 1:
                raise RuntimeError("failed to insert snap_angle")
    return text


def export_one(board: dict, pcbnew) -> dict:
    stem = board["stem"]
    src = origin_path(board)
    if not src.exists():
        raise FileNotFoundError(src)
    src_sha = sha256_file(src)
    dsn_path = DSN_DIR / f"{stem}.dsn"
    pcb_copy = PCB_DIR / f"{stem}.kicad_pcb"
    PCB_DIR.mkdir(parents=True, exist_ok=True)

    cmd = None
    if board["kind"] == "dsn_copy":
        shutil.copy2(src, dsn_path)
        cmd = f"cp {src} {dsn_path}"
        pcb_copy_s = None
    else:
        shutil.copy2(src, pcb_copy)
        board_obj = pcbnew.LoadBoard(str(pcb_copy))
        stripped = 0
        if board.get("strip_copper"):
            for item in list(board_obj.GetTracks()):
                board_obj.Delete(item)
                stripped += 1
            pcbnew.SaveBoard(str(pcb_copy), board_obj)
        if not pcbnew.ExportSpecctraDSN(board_obj, str(dsn_path)):
            raise RuntimeError(f"ExportSpecctraDSN failed for {stem}")
        text = dsn_path.read_text(encoding="utf-8")
        text = patch_dsn_text(
            text, board.get("plane_patch") or [], board.get("snap_angle_patch")
        )
        dsn_path.write_text(text, encoding="utf-8")
        cmd = (
            f"{KICAD_PY} pcbnew.ExportSpecctraDSN({pcb_copy.name}) "
            f"strip_copper={bool(board.get('strip_copper'))} "
            f"stripped={stripped} plane_patch={board.get('plane_patch')} "
            f"snap_angle_patch={board.get('snap_angle_patch')}"
        )
        pcb_copy_s = str(pcb_copy.relative_to(REPO))

    source = {
        "kind": board["kind"],
        "origin": str(src),
        "origin_sha256": src_sha,
        "origin_bytes": src.stat().st_size,
        "origin_mtime": int(src.stat().st_mtime),
        "kicad_pcb": pcb_copy_s,
        "export_cmd": cmd,
        "strip_copper": bool(board.get("strip_copper")),
        "plane_patch": board.get("plane_patch") or [],
        "snap_angle_patch": board.get("snap_angle_patch"),
        "why": board.get("why"),
        "kicad_version": "10.0.5",
    }
    return source


def inject_sources(manifest: dict, sources: dict) -> dict:
    for entry in manifest["fixtures"]:
        stem = entry["name"][: -len(".dsn")]
        if stem not in sources:
            raise SystemExit(f"manifest fixture {entry['name']} has no source record")
        entry["source"] = sources[stem]
    manifest["notes"] = list(manifest.get("notes") or []) + [
        "source is the G2.4 evidence field: origin path + sha256, export command, "
        "optional copper-strip / plane-type / snap_angle patches.",
        "This manifest is the out-of-sample tier (bench/fixtures-oos), not the 46-board suite.",
    ]
    return manifest


def write_manifest(sources: dict) -> None:
    sys.path.insert(0, str(REPO / "bench"))
    import manifest as fixture_manifest

    man = fixture_manifest.build_manifest(
        str(DSN_DIR),
        results_path=None,
        uncapped_dir=None,
        stability_dir=None,
    )
    # Drop non-DSN siblings if any leaked; build_manifest only glob *.dsn.
    man = inject_sources(man, sources)
    MANIFEST_OUT.write_text(fixture_manifest.dumps(man), encoding="utf-8")
    print(f"wrote {MANIFEST_OUT} ({len(man['fixtures'])} fixtures)")
    cov = man["coverage"]
    print(
        f"  signal_layers={cov['signal_layers']} "
        f"with_plane_layers={cov['with_plane_layers']} "
        f"snap={cov['snap_angle_declared']} "
        f"host_cad={cov['host_cad']} "
        f"preexisting_wiring={cov['with_preexisting_wiring']}"
    )


def main() -> int:
    _reexec_kicad_python()
    import wx  # type: ignore

    _app = wx.App(False)  # noqa: F841
    import pcbnew  # type: ignore

    catalog = json.loads(CATALOG.read_text(encoding="utf-8"))
    fetch_demos()
    sources = {}
    for board in catalog["boards"]:
        print(f"=== {board['stem']} ===")
        sources[board["stem"]] = export_one(board, pcbnew)
        dsn = DSN_DIR / f"{board['stem']}.dsn"
        print(f"  dsn {dsn.stat().st_size} bytes")
    SOURCES_OUT.write_text(json.dumps(sources, indent=2, ensure_ascii=False) + "\n")
    write_manifest(sources)
    # wx.App teardown can hang indefinitely; the files are already on disk.
    os._exit(0)


if __name__ == "__main__":
    raise SystemExit(main())
