"""Step 5:
1. sensor-hdi: SES wire width distribution; the 0.15mm track_width violators -> SES width?
2. sensor-hdi shorts: do same net-pairs short in BEFORE drc? fixed wires in DSN?
3. coldfire-unrouted /DTIN3 case: SES wire + angle check
4. per-violation off-angle membership (item pos ON an off-angle SES segment)
5. edge clearance measurement: track pos -> dist to KiCad Edge.Cuts vs DSN boundary
6. SES padstack shapes vs DSN padstack shapes (via size comparison)
"""
import collections
import json
import math
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import (BOARDS, G4, DSN, SES, load_drc, load_ses,
                    load_pcb, load_dsn, find_children, find_child, iter_nodes,
                    point_seg_dist, seg_angle, item_kind, dsn_resolution)

HERE = os.path.dirname(os.path.abspath(__file__))
d3 = json.load(open(os.path.join(HERE, "delta3.json")))


def off_angle_segs(board, tol_deg=0.5):
    ses = load_ses(board)
    snap90 = board == "kicad-coldfire-unrouted"
    step = 90.0 if snap90 else 45.0
    off, allsegs = [], []
    for (a, b, w, layer, net) in ses["wires"]:
        ang = seg_angle(a, b)
        dev = min((ang / step) % 1, 1 - (ang / step) % 1) * step
        allsegs.append((a, b, w, layer, net, dev > tol_deg))
        if dev > tol_deg:
            off.append((a, b, w, layer, net))
    return allsegs, off


print("=== 1. sensor-hdi SES wire widths + the 0.15 violators ===")
ses = load_ses("cla45-sensor-hdi")
wc = collections.Counter(round(w, 3) for a, b, w, l, n in ses["wires"])
print("  SES width histogram (mm):", dict(wc))
for v in d3["cla45-sensor-hdi"]["new"]:
    if v["type"] != "track_width":
        continue
    it = v["items"][0]
    p = (it["pos"]["x"], it["pos"]["y"])
    hits = [(w, n, l) for a, b, w, l, n in ses["wires"] if point_seg_dist(p, a, b) < 0.02]
    print("  width-viol", v["items"][0]["description"][:55], p, "-> SES:", hits[:2])

print("\n=== 2. sensor-hdi shorts: same net-pair in BEFORE? ===")
before = load_drc("cla45-sensor-hdi", "before")["violations"]

def short_nets(v):
    m = re.search(r"网络 (.+?) 和 (.+?)\)", v["description"])
    return (m.group(1), m.group(2)) if m else None

bshorts = collections.Counter()
for v in before:
    if v["type"] == "shorting_items":
        sn = short_nets(v)
        if sn:
            bshorts[tuple(sorted(sn))] += 1
for v in d3["cla45-sensor-hdi"]["new"]:
    if v["type"] != "shorting_items":
        continue
    sn = tuple(sorted(short_nets(v)))
    pts = [(round(it["pos"]["x"], 3), round(it["pos"]["y"], 3)) for it in v["items"]]
    print("  NEW short", sn, "before-count:", bshorts.get(sn, 0), pts)

# DSN fixed wires
txt = open(os.path.join(DSN, "cla45-sensor-hdi.dsn")).read()
print("  sensor-hdi DSN '(type fix)' count:", txt.count("(type fix)"), " wires total:", txt.count("(wire"))

print("\n=== 3. coldfire-unrouted /DTIN3 ===")
ses = load_ses("kicad-coldfire-unrouted")
p = (102.678, 120.142)
for a, b, w, l, n in ses["wires"]:
    if point_seg_dist(p, a, b) < 0.05:
        ang = seg_angle(a, b)
        print("  SES wire:", n, l, "w=", w, a, b, "angle=", round(ang, 2))

print("\n=== 4. off-angle membership for NEW violations (coldfire, coldfire-unrouted, sensor-hdi) ===")
for board in ["kicad-coldfire", "kicad-coldfire-unrouted", "cla45-sensor-hdi"]:
    allsegs, off = off_angle_segs(board)
    n_on = n_off = n_none = 0
    for v in d3[board]["new"]:
        if any("填充区" in it["description"] for it in v["items"]):
            continue
        hit = False
        for it in v["items"]:
            if "走线" not in it["description"]:
                continue
            p = (it["pos"]["x"], it["pos"]["y"])
            for a, b, w, l, n, is_off in allsegs:
                if point_seg_dist(p, a, b) < 0.03 and is_off:
                    hit = True
                    break
        if hit:
            n_off += 1
        else:
            n_on += 1
    print(f"  {board}: non-zone NEW with a track-item ON an off-angle segment: {n_off}, others: {n_on}")

print("\n=== 5. edge clearance measurement ===")


def kicad_edges(board):
    t = load_pcb(board, "before")
    segs = []
    for tag in ["gr_line", "gr_arc", "gr_rect", "gr_poly"]:
        for g in find_children(t, tag):
            layer = find_child(g, "layer")
            if not layer or "Edge.Cuts" not in str(layer[1]):
                continue
            if tag == "gr_line":
                st, en = find_child(g, "start"), find_child(g, "end")
                segs.append(((float(st[1]), float(st[2])), (float(en[1]), float(en[2]))))
            elif tag == "gr_arc":
                st, mid, en = (find_child(g, "start"), find_child(g, "mid"),
                               find_child(g, "end"))
                segs.append(("ARC", (float(st[1]), float(st[2])),
                             (float(mid[1]), float(mid[2])), (float(en[1]), float(en[2]))))
    return segs


def dsn_boundary(board):
    t = load_dsn(board)
    res = dsn_resolution(t)
    out = []
    struct = find_child(t, "structure")
    for bnd in find_children(struct, "boundary"):
        for pth in find_children(bnd, "path"):
            coords = [float(c) / res for c in pth[3:] if isinstance(c, (int, float))]
            pts = [(coords[i], -coords[i + 1]) for i in range(0, len(coords) - 1, 2)]
            out.extend(zip(pts, pts[1:] + pts[:1]))
        for rct in find_children(bnd, "rect"):
            x1, y1, x2, y2 = [float(v) / res for v in rct[2:6]]
            pts = [(x1, -y1), (x2, -y1), (x2, -y2), (x1, -y2)]
            out.extend(zip(pts, pts[1:] + pts[:1]))
    return out


def arc_dist(p, arc):
    # sample the arc through 3 points
    _, a, m, b = arc
    # circle through 3 points
    ax, ay = a; mx, my = m; bx, by = b
    d = 2 * (ax * (my - by) + mx * (by - ay) + bx * (ay - my))
    if abs(d) < 1e-12:
        return min(point_seg_dist(p, a, b), 1e9)
    ux = ((ax**2 + ay**2) * (my - by) + (mx**2 + my**2) * (by - ay) + (bx**2 + by**2) * (ay - my)) / d
    uy = ((ax**2 + ay**2) * (bx - mx) + (mx**2 + my**2) * (ax - bx) + (bx**2 + by**2) * (mx - ax)) / d
    r = math.hypot(ax - ux, ay - uy)
    # sample 64 pts, keep those on the minor arc a->m->b
    best = 1e9
    pts = []
    N = 128
    # param: angles
    a0 = math.atan2(ay - uy, ax - ux)
    am = math.atan2(my - uy, mx - ux)
    a1 = math.atan2(by - uy, bx - ux)
    # unwrap so a0 < am < a1 or reverse
    def unwrap(a0, am, a1):
        while am < a0: am += 2 * math.pi
        while a1 < am: a1 += 2 * math.pi
        if a1 - a0 > 2 * math.pi:
            a0, a1 = a1, a0 + 2 * math.pi
            while am < a0: am += 2 * math.pi
            while am > a1: am -= 2 * math.pi
        return a0, am, a1
    a0, am, a1 = unwrap(a0, am, a1)
    for i in range(N + 1):
        t = a0 + (a1 - a0) * i / N
        pts.append((ux + r * math.cos(t), uy + r * math.sin(t)))
    for q, q2 in zip(pts, pts[1:]):
        best = min(best, point_seg_dist(p, q, q2))
    return best


for board in ["cla45-sensor-l6", "cla45-sensor-hdi"]:
    kedges = kicad_edges(board)
    bedges = dsn_boundary(board)
    ses = load_ses(board)
    print(f"  -- {board}: kicad edge segs={len(kedges)} dsn boundary segs={len(bedges)}")
    for v in d3[board]["new"]:
        if v["type"] != "copper_edge_clearance":
            continue
        tr = [it for it in v["items"] if "走线" in it["description"] or "过孔" in it["description"]]
        ed = [it for it in v["items"] if "Edge.Cuts" in it["description"]]
        if not tr:
            continue
        p = (tr[0]["pos"]["x"], tr[0]["pos"]["y"])
        # width of our ses wire at p
        w = None
        for a, b, ww, l, n in ses["wires"]:
            if point_seg_dist(p, a, b) < 0.03:
                w = ww
                break
        dk = 1e9
        for e in kedges:
            if isinstance(e, tuple) and len(e) == 4:
                dk = min(dk, arc_dist(p, e))
            else:
                dk = min(dk, point_seg_dist(p, e[0], e[1]))
        db = min((point_seg_dist(p, a, b) for a, b in bedges), default=1e9)
        print(f"    track@{p} w={w} edgeitem=({ed[0]['pos']['x']},{ed[0]['pos']['y']}) "
              f"dist_to_kicad_edge_center={dk:.4f} dist_to_dsn_boundary={db:.4f} | {v['description'][:70]}")

print("\n=== 6. padstack compare (sensor-hdi shorts) ===")
ses = load_ses("cla45-sensor-hdi")
for target in [(115.55, 100.95), (83.94, 98.86), (110.6, 113.92)]:
    for x, y, ps, net in ses["vias"]:
        if math.hypot(x - target[0], y - target[1]) < 0.02:
            print("  SES via", net, ps, (x, y))
# padstack shapes from SES library_out
tree = None
with open(os.path.join(SES, "cla45-sensor-hdi.rust.ses")) as f:
    from common import parse_sexpr
    tree = parse_sexpr(f.read())
res = 10000.0
for ps in iter_nodes(tree, "padstack"):
    shapes = []
    for sh in find_children(ps, "shape"):
        for c in find_children(sh, "circle"):
            shapes.append((str(c[1]), float(c[2]) / res))
    print("  SES padstack", ps[1], shapes)
