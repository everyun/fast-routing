"""Step 3: robust delta matching (greedy nearest by item positions, two passes)
+ add-only import check (segment/via counts before vs after vs SES)
+ DSN zone content check.
Pass 1: same type, same item count, sorted per-point distance < 0.02 mm  -> identical
Pass 2: same type, same unordered net multiset, all item pos within 0.5 mm -> re-reported (churn)
Remainder: genuinely new.
"""
import collections
import json
import math
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import (BOARDS, G4, DSN, load_drc, load_ses, load_pcb,
                    find_children, item_kind)


def nets_of(v):
    out = []
    for it in v["items"]:
        m = re.search(r"\[([^\]]+)\]", it["description"])
        d = it["description"]
        if "Edge.Cuts" in d:
            out.append("<edge>")
        elif "填充区" in d:
            out.append("<zone:" + (m.group(1) if m else "?") + ">")
        else:
            out.append(m.group(1) if m else d[:20])
    return sorted(out)


def pts_of(v):
    return sorted((it["pos"]["x"], it["pos"]["y"]) for it in v["items"])


def max_pt_dist(p1, p2):
    if len(p1) != len(p2):
        return 1e9
    return max(math.hypot(a[0] - b[0], a[1] - b[1]) for a, b in zip(p1, p2)) if p1 else 0.0


def delta3(board):
    before = load_drc(board, "before")["violations"]
    after = load_drc(board, "after")["violations"]
    by_type = collections.defaultdict(list)
    for v in before:
        by_type[v["type"]].append(v)
    identical, churn, new = [], [], []
    for v in after:
        pool = by_type.get(v["type"], [])
        pts = pts_of(v)
        # pass 1
        best, bi = 1e9, -1
        for i, w in enumerate(pool):
            d = max_pt_dist(pts, pts_of(w))
            if d < best:
                best, bi = d, i
        if bi >= 0 and best < 0.02:
            identical.append((v, pool.pop(bi)))
            continue
        # pass 2: same net multiset, loose position
        nets = nets_of(v)
        best, bi = 1e9, -1
        for i, w in enumerate(pool):
            if nets_of(w) != nets:
                continue
            d = max_pt_dist(pts, pts_of(w)) if len(pts) == len(pts_of(w)) else 1e9
            if d < best:
                best, bi = d, i
        if bi >= 0 and best < 0.5:
            churn.append((v, pool.pop(bi)))
        else:
            new.append(v)
    removed = [v for lst in by_type.values() for v in lst]
    return identical, churn, new, removed


result = {}
for b in BOARDS:
    identical, churn, new, removed = delta3(b)
    cn = collections.Counter(v["type"] for v in new)
    cr = collections.Counter(v["type"] for v in removed)
    cc = collections.Counter(v["type"] for v, _ in churn)
    print(f"== {b}: identical={len(identical)} churn={len(churn)} NEW={len(new)} removed-left={len(removed)}")
    print("   NEW:", dict(cn))
    print("   churn:", dict(cc))
    print("   removed-left:", dict(cr))
    result[b] = {"new": new, "churn": [(a, w) for a, w in churn],
                 "removed_left": removed}

    # add-only check
    tb, ta = load_pcb(b, "before"), load_pcb(b, "after")
    nb = (len(find_children(tb, "segment")), len(find_children(tb, "via")))
    na = (len(find_children(ta, "segment")), len(find_children(ta, "via")))
    ses = load_ses(b)
    print(f"   segs {nb[0]}->{na[0]} (+{na[0]-nb[0]}, ses wires={len(ses['wires'])}) "
          f"vias {nb[1]}->{na[1]} (+{na[1]-nb[1]}, ses vias={len(ses['vias'])})")

# DSN zone content
for b in BOARDS:
    txt = open(os.path.join(DSN, b + ".dsn")).read()
    hits = {k: len(re.findall(rf"\({k}[\s)]", txt)) for k in
            ["plane", "polygon", "keepout", "place_keepout", "wire_keepout", "via_keepout", "window"]}
    print(b, "DSN zone-ish tokens:", hits)

with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "delta3.json"), "w") as f:
    json.dump(result, f, ensure_ascii=False, indent=1)
print("wrote delta3.json")
