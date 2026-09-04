"""Step 2: annotate every NEW violation with evidence:
- whether involved vias/tracks are ours (present in our SES) or pre-existing
- whether positions sit on off-angle SES segments (45deg boards / 90deg board)
- zone fill polygons before vs after (refill check)
Outputs annotated.json and prints group summaries.
"""
import collections
import hashlib
import json
import math
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import (BOARDS, G4, delta_violations, item_kind, load_ses, load_pcb,
                    seg_angle, point_seg_dist, find_children, find_child)

NUM = r'([\d.]+) mm; 实际 ([\d.]+) mm'
RE_REQ_ACT = re.compile(NUM)


def req_act(desc):
    m = RE_REQ_ACT.search(desc)
    return (float(m.group(1)), float(m.group(2))) if m else (None, None)


def item_net(desc):
    m = re.search(r'\[([^\]]+)\]', desc)
    return m.group(1) if m else None


def load_kicad_copper(board, when):
    """segments/vias/pads from a .kicad_pcb: positions in mm."""
    tree = load_pcb(board, when)
    segs = []
    for s in find_children(tree, "segment"):
        st, en = find_child(s, "start"), find_child(s, "end")
        net = find_child(s, "net")
        segs.append(((float(st[1]), float(st[2])), (float(en[1]), float(en[2])),
                     net[1] if net else None))
    vias = []
    for v in find_children(tree, "via"):
        at = find_child(v, "at")
        net = find_child(v, "net")
        vias.append((float(at[1]), float(at[2]), net[1] if net else None))
    pads = []
    for fp in find_children(tree, "footprint"):
        fat = find_child(fp, "at")
        fx, fy = float(fat[1]), float(fat[2])
        frot = float(fat[3]) if len(fat) > 3 else 0.0
        cr, sr = math.cos(math.radians(frot)), math.sin(math.radians(frot))
        for p in find_children(fp, "pad"):
            at = find_child(p, "at")
            net = find_child(p, "net")
            px, py = float(at[1]), float(at[2])
            # KiCad pad pos: rotate then translate
            gx = fx + px * cr - py * sr
            gy = fy + px * sr + py * cr
            pads.append((gx, gy, net[1] if net else None, str(p[1])))
    return {"segs": segs, "vias": vias, "pads": pads}


def zone_fill_hash(board, when):
    tree = load_pcb(board, when)
    out = []
    for z in find_children(tree, "zone"):
        pts = []
        for poly in find_children(z, "filled_polygon"):
            pts.append(repr(poly))
        h = hashlib.md5("|".join(pts).encode()).hexdigest()[:10]
        name = find_child(z, "name")
        layer = find_child(z, "layers")
        out.append((name[1] if name else "", repr(layer), h, len(pts)))
    return out


report = {}
for b in BOARDS:
    new, removed = delta_violations(b)
    ses = load_ses(b)
    snap90 = (b == "kicad-coldfire-unrouted")
    # off-angle segments
    off = []
    for (a, bb, w, layer, net) in ses["wires"]:
        ang = seg_angle(a, bb)
        step = 90.0 if snap90 else 45.0
        dev = min((ang / step) % 1, 1 - (ang / step) % 1) * step
        if dev > 0.5:  # degrees
            off.append((a, bb, layer, net))
    before_copper = load_kicad_copper(b, "before")
    bvias = [(x, y) for x, y, n in before_copper["vias"]]
    bpads = [(x, y) for x, y, n, t in before_copper["pads"]]

    def near_ses_via(p, tol=0.02):
        return any(math.hypot(p[0] - x, p[1] - y) < tol for x, y, *_ in ses["vias"])

    def near_before_via(p, tol=0.02):
        return any(math.hypot(p[0] - x, p[1] - y) < tol for x, y in bvias)

    def near_before_pad(p, tol=0.02):
        return any(math.hypot(p[0] - x, p[1] - y) < tol for x, y in bpads)

    def near_ses_wire(p, tol=0.06):
        return any(point_seg_dist(p, a, bb) < tol for a, bb, *_ in ses["wires"])

    def near_off_angle(p, tol=0.15):
        return any(point_seg_dist(p, a, bb) < tol for a, bb, *_ in off)

    ann = []
    for v in new:
        ra, ac = req_act(v["description"])
        items = []
        for it in v["items"]:
            p = (it["pos"]["x"], it["pos"]["y"])
            k = item_kind(it["description"])
            e = {"kind": k, "pos": p, "net": item_net(it["description"]),
                 "desc": it["description"][:80]}
            if k == "via":
                e["in_our_ses"] = near_ses_via(p)
                e["preexisting"] = near_before_via(p)
            elif k == "track":
                e["on_our_seg"] = near_ses_wire(p)
            elif k == "pad":
                e["preexisting"] = near_before_pad(p)
            e["on_off_angle"] = near_off_angle(p)
            items.append(e)
        ann.append({"type": v["type"], "desc": v["description"],
                    "required": ra, "actual": ac, "items": items})

    # summaries
    grp = collections.Counter()
    for v in ann:
        kinds = tuple(sorted(i["kind"] for i in v["items"]))
        ours = any(i.get("in_our_ses") or i.get("on_our_seg") for i in v["items"])
        offhit = any(i["on_off_angle"] for i in v["items"])
        grp[(v["type"], kinds, "ours" if ours else "?", "offangle" if offhit else "")] += 1
    print(f"== {b}: ses vias={len(ses['vias'])} ses segs={len(ses['wires'])} off-angle segs={len(off)}")
    for k, n in grp.most_common():
        print("   ", n, k)
    print("    zone fills before:", zone_fill_hash(b, "before"))
    print("    zone fills after: ", zone_fill_hash(b, "after"))

    report[b] = {"new_annotated": ann, "removed": removed,
                 "ses_via_count": len(ses["vias"]), "off_angle_segs": len(off)}

with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "annotated.json"), "w") as f:
    json.dump(report, f, ensure_ascii=False, indent=1)
print("wrote annotated.json")
