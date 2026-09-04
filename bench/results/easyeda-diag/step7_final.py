"""Step 7: final per-violation classification evidence.
1. power-io: for each non-zone NEW violation, are ALL items pre-existing in before.kicad_pcb?
2. coldfire: for each of 14 NEW, are items our fresh routing? on off-angle seg? angles?
3. coldfire-unrouted: angle of the 'MOD1' text track.
4. sensor-hdi: for each non-zone NEW violation, are all copper items DSN pre-route / static pad?
   (vs our fresh). Also DSN wiring width histogram.
5. sensor-l6 & sensor-hdi: full DSN keepout dump + distance from violating tracks to keepout.
"""
import collections
import json
import math
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import (DSN, load_drc, load_ses, load_pcb, load_dsn, find_children,
                    find_child, iter_nodes, point_seg_dist, seg_angle,
                    dsn_resolution)

HERE = os.path.dirname(os.path.abspath(__file__))
d3 = json.load(open(os.path.join(HERE, "delta3.json")))


def has_zone(v):
    return any("填充区" in it["description"] for it in v["items"])


class CopperDB:
    def __init__(self, board, when):
        t = load_pcb(board, when)
        self.segs = []
        for s in find_children(t, "segment"):
            st, en = find_child(s, "start"), find_child(s, "end")
            self.segs.append(((float(st[1]), float(st[2])), (float(en[1]), float(en[2]))))
        self.vias = []
        for v in find_children(t, "via"):
            at = find_child(v, "at")
            self.vias.append((float(at[1]), float(at[2])))
        self.pads = []
        for fp in find_children(t, "footprint"):
            fat = find_child(fp, "at")
            fx, fy = float(fat[1]), float(fat[2])
            rot = math.radians(float(fat[3]) if len(fat) > 3 else 0.0)
            cr, sr = math.cos(rot), math.sin(rot)
            for p in find_children(fp, "pad"):
                at = find_child(p, "at")
                px, py = float(at[1]), float(at[2])
                self.pads.append((fx + px * cr - py * sr, fy + px * sr + py * cr))

    def has_track_at(self, p, tol=0.03):
        return any(point_seg_dist(p, a, b) < tol for a, b in self.segs)

    def has_via_at(self, p, tol=0.02):
        return any(math.hypot(p[0] - x, p[1] - y) < tol for x, y in self.vias)

    def has_pad_at(self, p, tol=0.02):
        return any(math.hypot(p[0] - x, p[1] - y) < tol for x, y in self.pads)


def item_preexisting(it, db):
    d = it["description"]
    p = (it["pos"]["x"], it["pos"]["y"])
    if "走线" in d:
        return db.has_track_at(p)
    if "过孔" in d or "埋孔" in d or "盲孔" in d:
        return db.has_via_at(p)
    if "焊盘" in d:
        return db.has_pad_at(p)
    return None  # text, edge, etc.


def dsn_wiring(board):
    t = load_dsn(board)
    res = dsn_resolution(t)
    wires, vias = [], []
    for w in iter_nodes(t, "wire"):
        p = find_child(w, "path")
        if not p:
            continue
        coords = [float(c) / res for c in p[3:] if isinstance(c, (int, float))]
        pts = [(coords[i], -coords[i + 1]) for i in range(0, len(coords) - 1, 2)]
        wires.append((pts, float(p[2]) / res))
    for v in iter_nodes(t, "via"):
        if len(v) > 3 and isinstance(v[2], (int, float)):
            vias.append((float(v[2]) / res, -float(v[3]) / res))
    return wires, vias


def on_dsn(p, wires, tol=0.03):
    return any(point_seg_dist(p, a, b) < tol
               for pts, w in wires for a, b in zip(pts, pts[1:]))


print("=== 1. power-io non-zone NEW: all items pre-existing? ===")
db = CopperDB("cla45-power-io-l6", "before")
n_all_pre = n_any = 0
for v in d3["cla45-power-io-l6"]["new"]:
    if has_zone(v):
        continue
    pres = [item_preexisting(it, db) for it in v["items"]]
    if all(p is True for p in pres):
        n_all_pre += 1
    else:
        n_any += 1
        print("  NOT-all-pre:", v["description"][:60],
              [(it["description"][:45], pr) for it, pr in zip(v["items"], pres)])
print(f"  all-pre-existing: {n_all_pre}, involving-new-copper: {n_any}")

print("\n=== 2. coldfire 14 NEW: fresh routing? off-angle? ===")
ses = load_ses("kicad-coldfire")
dbb = CopperDB("kicad-coldfire", "before")
for v in d3["kicad-coldfire"]["new"]:
    info = []
    for it in v["items"]:
        d = it["description"]
        p = (it["pos"]["x"], it["pos"]["y"])
        if "走线" in d:
            ang = None
            for a, b, w, l, n in ses["wires"]:
                if point_seg_dist(p, a, b) < 0.03:
                    ang = seg_angle(a, b)
                    wdt = w
                    break
            pre = dbb.has_track_at(p)
            info.append(f"track pre={pre} angle={ang and round(ang,1)} w={ang is not None and wdt}")
        elif "过孔" in d:
            info.append(f"via pre={dbb.has_via_at(p)}")
        elif "焊盘" in d:
            info.append(f"pad pre={dbb.has_pad_at(p)}")
    print("  ", v["description"][:55], info)

print("\n=== 3. coldfire-unrouted MOD1 track angle ===")
ses = load_ses("kicad-coldfire-unrouted")
for a, b, w, l, n in ses["wires"]:
    if point_seg_dist((112.0837, 94.7086), a, b) < 0.05:
        print("  wire", n, l, w, a, b, "angle", round(seg_angle(a, b), 2))

print("\n=== 4. sensor-hdi non-zone NEW: DSN pre-route or fresh? ===")
wires, dvias = dsn_wiring("cla45-sensor-hdi")
wh = collections.Counter(round(w, 3) for pts, w in wires)
print("  DSN wiring width histogram:", dict(wh))
ses = load_ses("cla45-sensor-hdi")
cnt = collections.Counter()
for v in d3["cla45-sensor-hdi"]["new"]:
    if has_zone(v):
        continue
    all_dsn = True
    for it in v["items"]:
        d = it["description"]
        p = (it["pos"]["x"], it["pos"]["y"])
        if "走线" in d:
            if not on_dsn(p, wires):
                all_dsn = False
        elif "过孔" in d or "埋孔" in d or "盲孔" in d:
            if not any(math.hypot(p[0] - x, p[1] - y) < 0.02 for x, y in dvias):
                all_dsn = False
        elif "焊盘" in d or "Edge.Cuts" in d or "文本" in d or "多边形" in d:
            pass
    kinds = v["type"]
    cnt[(kinds, "all-dsn-preroute" if all_dsn else "has-fresh-copper")] += 1
for k, n in cnt.most_common():
    print("  ", n, k)

print("\n=== 5. DSN keepouts vs violating tracks ===")
for board in ["cla45-sensor-l6", "cla45-sensor-hdi"]:
    t = load_dsn(board)
    res = dsn_resolution(t)
    print(f"-- {board}")
    keeps = []
    for k in iter_nodes(t, "keepout"):
        for sh in k[2:]:
            if isinstance(sh, list):
                if sh[0] == "polygon":
                    coords = [float(c) / res for c in sh[3:] if isinstance(c, (int, float))]
                    pts = [(coords[i], -coords[i + 1]) for i in range(0, len(coords) - 1, 2)]
                    xs = [p[0] for p in pts]
                    ys = [p[1] for p in pts]
                    print(f"  keepout polygon layer={sh[1]} npts={len(pts)} "
                          f"bbox=({min(xs):.2f},{min(ys):.2f})-({max(xs):.2f},{max(ys):.2f})")
                    keeps.append(("poly", pts))
                elif sh[0] == "circle":
                    cx = float(sh[3]) / res if len(sh) > 3 else 0
                    cy = -float(sh[4]) / res if len(sh) > 4 else 0
                    print(f"  keepout circle layer={sh[1]} dia={float(sh[2])/res} center=({cx},{cy})")
    for v in d3[board]["new"]:
        if v["type"] != "copper_edge_clearance":
            continue
        tr = [it for it in v["items"] if "走线" in it["description"] or "过孔" in it["description"]][0]
        p = (tr["pos"]["x"], tr["pos"]["y"])
        dmin = 1e9
        for kind, pts in keeps:
            closed = pts + [pts[0]]
            for a, b in zip(closed, closed[1:]):
                dmin = min(dmin, point_seg_dist(p, a, b))
        print(f"    track@{p} dist_to_keepout_polygon={dmin:.3f}")
