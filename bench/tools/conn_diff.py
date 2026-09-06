#!/usr/bin/env python3
"""Per-connection settle differ for the FRLAB/FRCONN conn records (task settle-diff & deadend-157).

Both sides emit one line per pass-level autoroute attempt:

    FRLAB conn pass=1 net=5 src=Pin@123,456 item=42 exp=229 ok=ROUTED traces=1 path=… vias=0:
    FRCONN conn pass=1 net=5 src=Pin@123,456 item=7 exp=231 ok=ROUTED traces=1 path=… vias=0:

(lab jar `app.freerouting.frlab.FrLab` vs `fr_router::batch::emit_conn_landing`).

Item ids are NOT comparable across the implementations; the match key is
`(pass, net, src anchor)` — src anchors are board coordinates.

Matching modes:
  - Default: Key-based matching (`(pass, net, src anchor)`). This handles out-of-order
    or differently sequenced work lists and correctly pairs identical connections.
    Reports matched connections with status/expansion comparison, plus explicit lists
    of connections present only in `ours` or only in `java`.
  - Positional (`--positional`): Matches records index-by-index within each pass
    (legacy mode).

Usage:
    python3 bench/tools/conn_diff.py ours.log java.log [--pass N] [--context K] [--json] [--positional]
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass

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


def diff_pass_positional(ours: list[Conn], java: list[Conn], context: int) -> dict:
    """Positional index-by-index comparison within one pass (legacy mode)."""
    n = min(len(ours), len(java))
    first = None
    for i in range(n):
        if ours[i].key != java[i].key:
            first = i
            break
    if first is None and len(ours) != len(java):
        first = n
    result = {
        "mode": "positional",
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
        for i in range(n):
            o, j = ours[i], java[i]
            if o.ok != j.ok or o.path_ends != j.path_ends:
                result["first_detail_divergence"] = i
                result["detail"] = {"ours": describe(o), "java": describe(j)}
                break
    return result


def diff_pass_keyed(ours: list[Conn], java: list[Conn]) -> dict:
    """Key-based matching on `(net, src anchor)` within one pass."""
    ours_by_key: dict[tuple[int, str], list[Conn]] = {}
    for c in ours:
        ours_by_key.setdefault(c.key, []).append(c)

    java_by_key: dict[tuple[int, str], list[Conn]] = {}
    for c in java:
        java_by_key.setdefault(c.key, []).append(c)

    all_keys = list(dict.fromkeys(list(ours_by_key.keys()) + list(java_by_key.keys())))

    matched = []
    ours_only = []
    java_only = []
    status_mismatches = []
    large_exp_diffs = []

    for k in all_keys:
        o_list = ours_by_key.get(k, [])
        j_list = java_by_key.get(k, [])
        if o_list and not j_list:
            for c in o_list:
                ours_only.append(describe(c))
        elif j_list and not o_list:
            for c in j_list:
                java_only.append(describe(c))
        else:
            # Present on both sides; match corresponding attempts (primary = first attempt)
            for idx in range(max(len(o_list), len(j_list))):
                o = o_list[idx] if idx < len(o_list) else None
                j = j_list[idx] if idx < len(j_list) else None
                if o is not None and j is not None:
                    matched.append({
                        "key": f"net={k[0]} src={k[1]}",
                        "ours": describe(o),
                        "java": describe(j),
                        "ours_ok": o.ok,
                        "java_ok": j.ok,
                        "ours_exp": o.exp,
                        "java_exp": j.exp,
                        "exp_diff": o.exp - j.exp,
                    })
                    if o.ok != j.ok:
                        status_mismatches.append({
                            "key": f"net={k[0]} src={k[1]}",
                            "ours": describe(o),
                            "java": describe(j),
                        })
                    if abs(o.exp - j.exp) > 5000:
                        large_exp_diffs.append({
                            "key": f"net={k[0]} src={k[1]}",
                            "diff": o.exp - j.exp,
                            "ours_exp": o.exp,
                            "java_exp": j.exp,
                            "ours_ok": o.ok,
                            "java_ok": j.ok,
                        })
                elif o is not None:
                    ours_only.append(describe(o))
                elif j is not None:
                    java_only.append(describe(j))

    large_exp_diffs.sort(key=lambda x: abs(x["diff"]), reverse=True)

    return {
        "mode": "keyed",
        "ours_total": len(ours),
        "java_total": len(java),
        "matched_count": len(matched),
        "ours_only_count": len(ours_only),
        "java_only_count": len(java_only),
        "status_mismatch_count": len(status_mismatches),
        "ours_only": ours_only,
        "java_only": java_only,
        "status_mismatches": status_mismatches,
        "large_exp_diffs": large_exp_diffs,
    }


def diff_pass(ours: list[Conn], java: list[Conn], context: int, positional: bool = False) -> dict:
    if positional:
        return diff_pass_positional(ours, java, context)
    return diff_pass_keyed(ours, java)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("ours")
    ap.add_argument("java")
    ap.add_argument("--pass", dest="only_pass", type=int, default=None)
    ap.add_argument("--context", type=int, default=2)
    ap.add_argument("--positional", action="store_true", help="Match index-by-index instead of by (net, src)")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()

    ours = parse(args.ours)
    java = parse(args.java)
    passes = sorted(set(ours) | set(java))
    if args.only_pass is not None:
        passes = [p for p in passes if p == args.only_pass]

    report = []
    for p in passes:
        r = diff_pass(ours.get(p, []), java.get(p, []), args.context, positional=args.positional)
        r["pass"] = p
        report.append(r)

    if args.json:
        print(json.dumps(report, indent=1))
        return 0

    exit_code = 0
    for r in report:
        if r.get("mode") == "positional":
            line = f"pass {r['pass']} (positional): ours={r['ours']} java={r['java']} aligned_prefix={r['positional_prefix']}"
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
        else:
            line = (
                f"pass {r['pass']} (keyed): matched={r['matched_count']} "
                f"ours_total={r['ours_total']} java_total={r['java_total']} "
                f"ours_only={r['ours_only_count']} java_only={r['java_only_count']} "
                f"status_mismatches={r['status_mismatch_count']}"
            )
            print(line)
            if r["status_mismatch_count"] > 0:
                exit_code = 1
                print(f"  STATUS MISMATCHES ({r['status_mismatch_count']}):")
                for sm in r["status_mismatches"][:10]:
                    print(f"    [{sm['key']}] ours: {sm['ours']} | java: {sm['java']}")
                if len(r["status_mismatches"]) > 10:
                    print(f"    ... and {len(r['status_mismatches']) - 10} more")

            if r["ours_only_count"] > 0:
                print(f"  OURS ONLY ({r['ours_only_count']}):")
                for item in r["ours_only"][:10]:
                    print(f"    {item}")
                if len(r["ours_only"]) > 10:
                    print(f"    ... and {len(r['ours_only']) - 10} more")

            if r["java_only_count"] > 0:
                print(f"  JAVA ONLY ({r['java_only_count']}):")
                for item in r["java_only"][:10]:
                    print(f"    {item}")
                if len(r["java_only"]) > 10:
                    print(f"    ... and {len(r['java_only']) - 10} more")

            if r["large_exp_diffs"]:
                print(f"  TOP EXPANSION DIFFERENCES (diff = ours - java):")
                for ed in r["large_exp_diffs"][:5]:
                    print(f"    [{ed['key']}] diff={ed['diff']:+d} (ours={ed['ours_exp']} {ed['ours_ok']} vs java={ed['java_exp']} {ed['java_ok']})")

    return exit_code


if __name__ == "__main__":
    sys.exit(main())
