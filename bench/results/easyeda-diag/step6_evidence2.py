"""Step 6:
1. DSN boundary vs KiCad Edge.Cuts for sensor-l6 / sensor-hdi (print both).
2. sensor-hdi: are short/track_width/clearance objects pre-routes from DSN wiring?
   (match violation item positions against DSN wiring wires/vias)
3. before-drc +1V2/CAM_D3_P shorts positions vs new ones.
4. sensor-hdi DSN network: per-net or per-class width 150?
"""
import json
import math
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import (DSN, load_drc, load_dsn, load_pcb, find_children, find_child,
                    iter_nodes, dsn_resolution, point_seg_dist)

HERE = os.path.dirname(os.path.abspath(__file__))
d3 = json.load(open(os.path.join(HERE, "delta3.json")))


def dsn_wiring(board):
    """DSN wiring wires/vias in KiCad mm coords (y negated)."""
    t = load_dsn(board)
    res = dsn_resolution(t)
    wires, vias = [], []
    for w in iter_nodes(t, "wire"):
        p = find_child(w, "path")
        if not p:
            continue
        coords = [float(c) / res for c in p[3:] if isinstance(c, (int, float))]
        pts = [(coords[i], -coords[i + 1]) for i in range(0, len(coords) - 1, 2)]
        n = find_child(w, "net")
        typ = find_child(w, "type")
        wires.append((pts, float(p[2]) / res, str(p[1]),
                      str(n[1]) if n else "", str(typ[1]) if typ else ""))
    for v in iter_nodes(t, "via"):
        # (via padstack x y ...) inside wiring section
        if len(v) > 3 and isinstance(v[2], (int, float)):
            n = find_child(v, "net")
            vias.append((float(v[2]) / res, -float(v[3]) / res, str(v[1]),
                         str(n[1]) if n else ""))
    return wires, vias


print("=== 1. boundaries ===")
for board in ["cla45-sensor-l6", "cla45-sensor-hdi"]:
    t = load_dsn(board)
    res = dsn_resolution(t)
    struct = find_child(t, "structure")
    print(f"-- {board}")
    for bnd in iter_nodes(t, "boundary"):
        print("  DSN boundary:", str(bnd)[:300])
    for k in iter_nodes(t, "keepout"):
        print("  DSN keepout:", str(k)[:200])
    pcb = load_pcb(board, "before")
    for g in find_children(pcb, "gr_line") + find_children(pcb, "gr_arc"):
        layer = find_child(g, "layer")
        if layer and "Edge.Cuts" in str(layer[1]):
            st = find_child(g, "start")
            en = find_child(g, "end")
            print("  KiCad edge", g[0], st[1:3] if st else "", en[1:3] if en else "")

print("\n=== 2. sensor-hdi: violation objects in DSN wiring? ===")
wires, dvias = dsn_wiring("cla45-sensor-hdi")
print(f"  DSN wiring: {len(wires)} wires, {len(dvias)} vias")


def on_dsn_wire(p, tol=0.03):
    return [(w, l, n, ty) for pts, w, l, n, ty in wires
            if any(point_seg_dist(p, a, b) < tol for a, b in zip(pts, pts[1:]))][:2]


def at_dsn_via(p, tol=0.02):
    return [(ps, n) for x, y, ps, n in dvias if math.hypot(x - p[0], y - p[1]) < tol]


targets = []
for v in d3["cla45-sensor-hdi"]["new"]:
    if any("填充区" in it["description"] for it in v["items"]):
        continue
    if v["type"] in ("shorting_items", "clearance", "hole_clearance", "tracks_crossing"):
        targets.append(v)
for v in targets:
    parts = []
    for it in v["items"]:
        d = it["description"]
        p = (it["pos"]["x"], it["pos"]["y"])
        if "走线" in d:
            m = on_dsn_wire(p)
            parts.append(f"track{'=DSN!' if m else '(ours)'} {d[:40]}")
        elif "过孔" in d or "埋孔" in d:
            m = at_dsn_via(p)
            parts.append(f"via{'=DSN!' if m else '(ours)'} {d[:40]}")
        else:
            parts.append(d[:30])
    print(f"  {v['type']}: {v['description'][:55]}")
    for s in parts:
        print("     ", s)

print("\n=== 3. before +1V2/CAM_D3_P shorts ===")
before = load_drc("cla45-sensor-hdi", "before")["violations"]
for v in before:
    if v["type"] == "shorting_items" and "CAM_D3_P" in v["description"]:
        print("  BEFORE:", v["description"][:60],
              [(it["description"][:40], (round(it["pos"]["x"], 3), round(it["pos"]["y"], 3))) for it in v["items"]])

print("\n=== 4. sensor-hdi DSN width 150 rules ===")
t = load_dsn("cla45-sensor-hdi")
res = dsn_resolution(t)
net = find_child(t, "network")
for cls in find_children(net, "class"):
    rule = find_child(cls, "rule")
    w = rule and find_child(rule, "width")
    cl = rule and find_child(rule, "clearance")
    print("  class", cls[1], "width", float(w[1]) / res if w else None,
          "clearance", float(cl[1]) / res if cl else None, "nets:", len(cls[2]) if isinstance(cls[2], list) else "many")
for n in find_children(net, "net"):
    rule = find_child(n, "rule")
    if rule:
        w = find_child(rule, "width")
        if w:
            print("  net", n[1], "has own width", float(w[1]) / res)
# DSN wiring width histogram
import collections
wh = collections.Counter(round(w, 3) for pts, w, l, n, ty in wires)
print("  DSN wiring width histogram:", dict(wh))
