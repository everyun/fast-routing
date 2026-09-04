"""Step 1: compute delta violations per board, verify counts vs the known table,
and break new violations down by item-kind pair."""
import collections
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import (BOARDS, delta_violations, item_kind, load_drc,
                    dsn_rules, dsn_snap_angle, load_dsn,
                    pcb_setup_constraints, pcb_netclass_clearances,
                    pcb_zone_clearances, load_pcb)

EXPECTED = {
    "cla45-power-io-l6": {"clearance": 238, "hole_clearance": 120},
    "kicad-coldfire-unrouted": {"clearance": 189, "hole_clearance": 92},
    "cla45-sensor-l6": {"copper_edge_clearance": 13},
    "cla45-sensor-hdi": {"hole_clearance": 42, "copper_edge_clearance": 16,
                         "clearance": -4, "shorting_items": -24,
                         "tracks_crossing": -4, "track_width": -14},
    "kicad-coldfire": {"clearance": 1},
}

out = {}
for b in BOARDS:
    new, removed = delta_violations(b)
    cnt_new = collections.Counter(v["type"] for v in new)
    cnt_rem = collections.Counter(v["type"] for v in removed)
    net = {t: cnt_new.get(t, 0) - cnt_rem.get(t, 0)
           for t in set(cnt_new) | set(cnt_rem)}
    print(f"== {b}: new={len(new)} removed={len(removed)}")
    print("   new by type:", dict(cnt_new))
    print("   removed by type:", dict(cnt_rem))
    print("   net:", net, " expected:", EXPECTED[b])

    # pair breakdown of new violations
    pairs = collections.Counter()
    descs = collections.defaultdict(collections.Counter)
    for v in new:
        kinds = tuple(sorted(item_kind(it["description"]) for it in v["items"]))
        pairs[(v["type"], kinds)] += 1
        descs[(v["type"], kinds)][v["description"]] += 1
    for k, n in pairs.most_common():
        print("   ", n, k)
        for d, c in descs[k].most_common(3):
            print("        ", c, d)

    # rules comparison data
    dsn = load_dsn(b)
    pcb = load_pcb(b, "before")
    out[b] = {
        "new": new,
        "removed": removed,
        "snap_angle": dsn_snap_angle(dsn),
        "dsn_rules": dsn_rules(dsn),
        "setup_constraints": pcb_setup_constraints(pcb),
        "netclass_clearances": pcb_netclass_clearances(pcb),
        "zone_clearances": pcb_zone_clearances(pcb),
    }
    print("    snap_angle:", out[b]["snap_angle"])
    print("    dsn pcb_rule:", out[b]["dsn_rules"]["pcb_rule"])
    print("    dsn classes:", out[b]["dsn_rules"]["classes"])
    print("    kicad setup constraints:", out[b]["setup_constraints"])
    print("    kicad netclass clearances:", out[b]["netclass_clearances"])
    print("    kicad zone clearances:", out[b]["zone_clearances"])

with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "delta.json"), "w") as f:
    json.dump(out, f, ensure_ascii=False, indent=1)
print("wrote delta.json")
