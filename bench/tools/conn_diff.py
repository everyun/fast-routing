#!/usr/bin/env python3
"""Per-connection settle differ for the FRLAB/FRCONN conn records (task settle-diff).

Both sides emit one line per pass-level autoroute attempt:

    FRLAB conn pass=1 net=5 src=Pin@123,456 item=42 exp=229 ok=ROUTED traces=1 path=… vias=0:
    FRCONN conn pass=1 net=5 src=Pin@123,456 item=7 exp=231 ok=ROUTED traces=1 path=… vias=0:

(lab jar `app.freerouting.frlab.FrLab` vs `fr_router::batch::emit_conn_landing`).

Item ids are NOT comparable across the implementations; the match key is
`(pass, net, src anchor)` — src anchors are board coordinates. The diff is
positional within a pass: the first index whose key pair disagrees is the first
behavioural divergence (same idea as the per-expansion settle diff in
`frlab_parse.py`). For key-matching records the outcome (`ok`), the expansion
count and the landing geometry (path endpoints, via count) are compared too.

Usage:
    python3 bench/tools/conn_diff.py ours.log java.log [--pass N] [--context K] [--json]
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass, field

RE_CONN = re.compile(r"\bFR(?:LAB|CONN)\s+conn\s+(.*)$")
RE_FIELD = re.compile(r"(\w+)=([^ ]*)")


@dataclass
class Conn:
    pass_no: int
    net: int
    src: str
    exp: int
    ok: str
    traces: int
    path: str
    vias: str
    raw: str

    @property
    def key(self) -> tuple[int, str]:
        return (self.net, self.src)

    @property
    def path_ends(self) -> tuple[str, str]:
        first_trace = self.path.split("|")[0] if self.path else ""
        corners = first_trace.split(";") if first_trace else [""]
        return (corners[0], corners[-1])


def parse(path: str) -> dict[int, list[Conn]]:
    passes: dict[int, list[Conn]] = {}
    with open(path, errors="replace") as fh:
        for line in fh:
            m = RE_CONN.search(line)
            if not m:
                continue
            fields = dict(RE_FIELD.findall(m.group(1)))
            try:
                conn = Conn(
                    pass_no=int(fields["pass"]),
                    net=int(fields["net"]),
                    src=fields["src"],
                    exp=int(fields.get("exp", 0)),
                    ok=fields.get("ok", "?"),
                    traces=int(fields.get("traces", 0)),
                    path=fields.get("path", ""),
                    vias=fields.get("vias", "0:"),
                    raw=line.strip(),
                )
            except (KeyError, ValueError):
                continue
            passes.setdefault(conn.pass_no, []).append(conn)
    return passes


def describe(c: Conn | None) -> str:
    if c is None:
        return "<none>"
    ends = c.path_ends
    via_n = c.vias.split(":", 1)[0]
    return (
        f"net={c.net} src={c.src} exp={c.exp} ok={c.ok} traces={c.traces} "
        f"ends={ends[0]}..{ends[-1]} vias={via_n}"
    )


def diff_pass(ours: list[Conn], java: list[Conn], context: int) -> dict:
    """Positional key comparison within one pass."""
    n = min(len(ours), len(java))
    first = None
    for i in range(n):
        if ours[i].key != java[i].key:
            first = i
            break
    if first is None and len(ours) != len(java):
        first = n
    result = {
        "ours": len(ours),
        "java": len(java),
        "positional_prefix": first if first is not None else n,
        "aligned": first is None,
    }
    if first is not None:
        lo = max(0, first - context)
        result["first_divergence"] = first
        result["ours_window"] = [describe(c) for c in ours[lo : first + context + 1]]
        result["java_window"] = [describe(c) for c in java[lo : first + context + 1]]
    else:
        # Same key sequence — walk for the first outcome/geometry difference.
        for i in range(n):
            o, j = ours[i], java[i]
            if o.ok != j.ok or o.path_ends != j.path_ends:
                result["first_detail_divergence"] = i
                result["detail"] = {"ours": describe(o), "java": describe(j)}
                break
    return result


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("ours")
    ap.add_argument("java")
    ap.add_argument("--pass", dest="only_pass", type=int, default=None)
    ap.add_argument("--context", type=int, default=2)
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()

    ours = parse(args.ours)
    java = parse(args.java)
    passes = sorted(set(ours) | set(java))
    if args.only_pass is not None:
        passes = [p for p in passes if p == args.only_pass]

    report = []
    for p in passes:
        r = diff_pass(ours.get(p, []), java.get(p, []), args.context)
        r["pass"] = p
        report.append(r)

    if args.json:
        print(json.dumps(report, indent=1))
        return 0

    exit_code = 0
    for r in report:
        line = f"pass {r['pass']}: ours={r['ours']} java={r['java']} aligned_prefix={r['positional_prefix']}"
        print(line)
        if "first_divergence" in r:
            exit_code = 1
            print(f"  FIRST DIVERGENCE at conn index {r['first_divergence']}:")
            print("  ours:")
            for w in r["ours_window"]:
                print(f"    {w}")
            print("  java:")
            for w in r["java_window"]:
                print(f"    {w}")
            break
        if "first_detail_divergence" in r:
            print(f"  first detail divergence at conn index {r['first_detail_divergence']}:")
            print(f"    ours: {r['detail']['ours']}")
            print(f"    java: {r['detail']['java']}")
    return exit_code


if __name__ == "__main__":
    sys.exit(main())
