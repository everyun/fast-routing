"""Step 4: detailed evidence gathering.
1. power-io: are all before segments/vias preserved in after? (junction split check)
2. power-io non-zone new clearance: nearest same-nets before violation (churn check)
3. coldfire-unrouted: the 2 non-zone new violations
4. coldfire: all 14 new vs 13 removed
5. sensor-hdi: non-zone new groups (shorts, crossings, track_width, hole cu-cu, edge)
6. edge clearance: distance from violating track to KiCad Edge.Cuts vs DSN boundary
"""
import collections
import json
import math
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import (BOARDS, G4, DSN, load_drc, load_ses, load_pcb, load_dsn,
                    find_children, find_child, iter_nodes, dsn_resolution,
                    point_seg_dist, item_kind)


def seg_set(board, when):
    t = load_pcb(board, when)
    out = set()
    for s in find_children(t, "segment"):
        st, en = find_child(s, "start"), find_child(s, "end")
        net = find_child(s, "net")
        out.add((round(float(st[1]), 4), round(float(st[2]), 4),
                 round(float(en[1]), 4), round(float(en[2]), 4),
                 str(net[1]) if net else ""))
    v = set()
    for s in find_children(t, "via"):
        at = find_child(s, "at")
        net = find_child(s, "net")
        v.add((round(float(at[1]), 4), round(float(at[2]), 4), str(net[1]) if net else ""))
    return out, v


print("=== 1. power-io segment preservation ===")
sb, vb = seg_set("cla45-power-io-l6", "before")
sa, va = seg_set("cla45-power-io-l6", "after")
print(f"before segs {len(sb)}, missing in after: {len(sb - sa)}")
print(f"before vias {len(vb)}, missing in after: {len(vb - va)}")
for m in sorted(sb - sa)[:10]:
    print("  missing seg:", m)

d3 = json.load(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "delta3.json")))


def nets_of(v):
    out = []
    for it in v["items"]:
        m = re.search(r"\[([^\]]+)\]", it["description"])
        d = it["description"]
        if "Edge.Cuts" in d:
            out.append("<edge>")
        elif "填充区" in d:
            out.append("<zone>")
        else:
            out.append(m.group(1) if m else d[:20])
    return sorted(out)


def pts_of(v):
    return sorted((it["pos"]["x"], it["pos"]["y"]) for it in v["items"])


def has_zone(v):
    return any("填充区" in it["description"] for it in v["items"])


print("\n=== 2. power-io non-zone NEW clearance: nearest same-nets BEFORE violation ===")
before = load_drc("cla45-power-io-l6", "before")["violations"]
n_matched = 0
nonzone_new = [v for v in d3["cla45-power-io-l6"]["new"] if not has_zone(v)]
for v in nonzone_new:
    pts = pts_of(v)
    nets = nets_of(v)
    best = 1e9
    for w in before:
        if w["type"] != v["type"] or nets_of(w) != nets:
            continue
        wp = pts_of(w)
        if len(wp) != len(pts):
            continue
        d = max(math.hypot(a[0] - b[0], a[1] - b[1]) for a, b in zip(pts, wp))
        best = min(best, d)
    if best < 0.5:
        n_matched += 1
    else:
        print("  NO before-match:", v["description"][:60], nets, pts)
print(f"  {n_matched}/{len(nonzone_new)} non-zone new have same-net before violation within 0.5mm")
print("  removed-left:", len(d3["cla45-power-io-l6"]["removed_left"]))

print("\n=== 3. coldfire-unrouted non-zone new ===")
for v in d3["kicad-coldfire-unrouted"]["new"]:
    if not has_zone(v):
        print("  ", v["type"], v["description"][:80])
        for it in v["items"]:
            print("     ", it["description"][:80], it["pos"])

print("\n=== 4. coldfire new vs removed-left ===")
for tag in ["new", "removed_left"]:
    print(" ", tag)
    for v in d3["kicad-coldfire"][tag]:
        print("   ", v["type"], v["description"][:70],
              [(it["description"][:45], (round(it["pos"]["x"], 3), round(it["pos"]["y"], 3))) for it in v["items"]])

print("\n=== 5. sensor-hdi non-zone new, by group ===")
grp = collections.defaultdict(list)
for v in d3["cla45-sensor-hdi"]["new"]:
    if has_zone(v):
        continue
    kinds = tuple(sorted(item_kind(it["description"]) for it in v["items"]))
    grp[(v["type"], kinds)].append(v)
for k, vs in sorted(grp.items(), key=lambda x: -len(x[1])):
    print(f"  {len(vs)}x {k}")
    for v in vs[:3]:
        print("     ", v["description"][:90])
        for it in v["items"]:
            print("        ", it["description"][:75], (round(it["pos"]["x"], 4), round(it["pos"]["y"], 4)))
