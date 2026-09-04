"""Shared helpers for DRC delta diagnosis (read-only analysis)."""
import json
import math
import os
import re

BASE = os.path.expanduser("~/fr-work/kicad-gate/bench/results/oos-ours/20260904-222720")
G4 = os.path.join(BASE, "g4-work")
SES = os.path.join(BASE, "ses")
DSN = os.path.expanduser("~/fr-work/kicad-gate/bench/fixtures-oos")

BOARDS = [
    "cla45-power-io-l6",
    "kicad-coldfire-unrouted",
    "cla45-sensor-l6",
    "cla45-sensor-hdi",
    "kicad-coldfire",
]


def load_drc(board, when):
    with open(os.path.join(G4, board + ".dsn", f"drc-{when}.json")) as f:
        return json.load(f)


def vkey(v, tol=0.01):
    """Match key: type + sorted rounded item positions (mm, rounded to tol)."""
    pts = sorted(
        (round(it["pos"]["x"] / tol), round(it["pos"]["y"] / tol))
        for it in v["items"]
    )
    return (v["type"], tuple(pts))


def delta_violations(board, tol=0.01):
    """Return (new, removed): violations in after but not before, and vice versa.

    Greedy multiset matching on (type, item positions within tol).
    """
    before = load_drc(board, "before")["violations"]
    after = load_drc(board, "after")["violations"]
    pool = {}
    for v in before:
        pool.setdefault(vkey(v, tol), []).append(v)
    new = []
    for v in after:
        k = vkey(v, tol)
        if pool.get(k):
            pool[k].pop()
        else:
            new.append(v)
    removed = [v for lst in pool.values() for v in lst]
    return new, removed


def item_kind(desc):
    """Categorize a KiCad DRC item description (zh locale)."""
    if "填充区" in desc:
        return "zone"
    if "过孔" in desc or "埋孔" in desc or "盲孔" in desc or "微孔" in desc:
        return "via"
    if "走线" in desc:
        return "track"
    if "弧线" in desc or "线段" in desc:
        return "edge"
    if "焊盘" in desc:
        return "pad"
    if "钻孔" in desc or "孔" in desc:
        return "hole"
    return desc[:12]


# ---------------- S-expression mini parser ----------------

def tokenize(text):
    # Specctra files contain a bare quote char in `(string_quote ")` which
    # would otherwise swallow the rest of the file as a string literal.
    text = text.replace('(string_quote ")', '(string_quote DQUOTE)')
    toks = re.findall(r'\(|\)|"(?:[^"\\]|\\.)*"|[^\s()"]+', text)
    return toks


def parse_sexpr(text):
    toks = tokenize(text)
    pos = 0

    def parse():
        nonlocal pos
        t = toks[pos]
        pos += 1
        if t == "(":
            lst = []
            while toks[pos] != ")":
                lst.append(parse())
            pos += 1
            return lst
        if t.startswith('"'):
            return t[1:-1]
        try:
            return int(t)
        except ValueError:
            try:
                return float(t)
            except ValueError:
                return t

    return parse()


def iter_nodes(tree, name):
    """Yield all sublists whose head == name (recursive)."""
    if isinstance(tree, list) and tree:
        if tree[0] == name:
            yield tree
        for ch in tree:
            if isinstance(ch, list):
                yield from iter_nodes(ch, name)


def find_child(node, name):
    for ch in node:
        if isinstance(ch, list) and ch and ch[0] == name:
            return ch
    return None


def find_children(node, name):
    return [ch for ch in node if isinstance(ch, list) and ch and ch[0] == name]


# ---------------- SES parsing ----------------

def ses_resolution(tree):
    """Units per mm from a Specctra (resolution <unit> <factor>) node list.

    (resolution um 10) => 10 units per um => 10000 units per mm.
    NOTE: KiCad's DSN export declares (resolution um 10) but actually writes
    plain um (1 unit = 1 um); our SES writer honors the declaration.
    Callers parsing these KiCad-exported DSNs must override with 1000.
    """
    for node in iter_nodes(tree, "resolution"):
        unit, factor = node[1], node[2]
        per_mm = {"um": 1000.0, "mm": 1.0, "mil": 39.3701, "inch": 0.0393701}[unit]
        return factor * per_mm
    return 1.0


def load_ses(board):
    """Parse our SES -> dict with vias [(x_mm,y_mm,padstack)] and
    wires [((x1,y1),(x2,y2),width_mm,layer,net)] in KiCad mm coords (y negated)."""
    with open(os.path.join(SES, board + ".rust.ses")) as f:
        tree = parse_sexpr(f.read())
    res = ses_resolution(tree)  # units per mm
    vias = []
    wires = []
    for node in iter_nodes(tree, "via"):
        # (via "padstack" x y (net ...) ...)
        name = node[1]
        x, y = float(node[2]) / res, -float(node[3]) / res
        net = ""
        n = find_child(node, "net")
        if n:
            net = str(n[1])
        vias.append((x, y, str(name), net))
    for node in iter_nodes(tree, "wire"):
        p = find_child(node, "path")
        if not p:
            continue
        layer = str(p[1])
        width = float(p[2]) / res
        coords = [float(c) for c in p[3:] if isinstance(c, (int, float))]
        net = ""
        n = find_child(node, "net")
        if n:
            net = str(n[1])
        pts = [(coords[i] / res, -coords[i + 1] / res) for i in range(0, len(coords) - 1, 2)]
        for a, b in zip(pts, pts[1:]):
            wires.append((a, b, width, layer, net))
    return {"vias": vias, "wires": wires}


def seg_angle(a, b):
    return math.degrees(math.atan2(b[1] - a[1], b[0] - a[0])) % 180.0


def point_seg_dist(p, a, b):
    ax, ay = a
    bx, by = b
    dx, dy = bx - ax, by - ay
    L2 = dx * dx + dy * dy
    if L2 == 0:
        return math.hypot(p[0] - ax, p[1] - ay)
    t = max(0, min(1, ((p[0] - ax) * dx + (p[1] - ay) * dy) / L2))
    return math.hypot(p[0] - ax - t * dx, p[1] - ay - t * dy)


# ---------------- DSN parsing ----------------

def load_dsn(board):
    with open(os.path.join(DSN, board + ".dsn")) as f:
        return parse_sexpr(f.read())


def dsn_resolution(tree):
    # KiCad exports DSN with (resolution um 10) but writes plain um values.
    return 1000.0


def dsn_rules(tree):
    """Extract clearance rules: pcb default rule + per-class rules (in mm)."""
    res = dsn_resolution(tree)
    out = {"pcb_rule": [], "classes": {}}
    struct = find_child(tree, "structure")
    if struct:
        rule = find_child(struct, "rule")
        if rule:
            for c in find_children(rule, "clearance"):
                out["pcb_rule"].append((float(c[1]) / res, str(c[2:]) if len(c) > 2 else "all"))
    net = find_child(tree, "network")
    if net:
        for cls in find_children(net, "class"):
            name = str(cls[1])
            rules = []
            rule = find_child(cls, "rule")
            if rule:
                for c in find_children(rule, "clearance"):
                    rules.append(float(c[1]) / res)
            w = rule and find_child(rule, "width")
            out["classes"][name] = {
                "clearances_mm": rules,
                "width_mm": float(w[1]) / res if w else None,
            }
    return out


def dsn_snap_angle(tree):
    struct = find_child(tree, "structure")
    if struct:
        for ch in struct:
            if isinstance(ch, list) and ch and ch[0] == "snap_angle":
                return str(ch[1])
    return None


# ---------------- kicad_pcb parsing ----------------

def load_pcb(board, when="before"):
    with open(os.path.join(G4, board + ".dsn", f"{when}.kicad_pcb")) as f:
        return parse_sexpr(f.read())


def pcb_setup_constraints(tree):
    setup = find_child(tree, "setup")
    keys = [
        "min_clearance", "min_track_width", "min_via_diameter", "min_via_annular_width",
        "hole_to_hole_clearance", "hole_clearance", "copper_edge_clearance",
        "min_microvia_diameter", "min_hole_diameter", "min_silk_clearance",
        "solder_mask_min_width", "solder_mask_to_copper_clearance",
    ]
    out = {}
    if setup:
        for k in keys:
            n = find_child(setup, k)
            if n:
                out[k] = n[1]
    return out


def pcb_netclass_clearances(tree):
    out = {}
    for nc in find_children(tree, "net_class"):
        name = str(nc[1])
        cl = find_child(nc, "clearance")
        out[name] = cl[1] if cl else None
    dflt = find_child(tree, "net_class")  # first is usually Default
    return out


def pcb_zone_clearances(tree):
    """zone name/net -> (clearance, min_thickness)."""
    out = []
    for z in find_children(tree, "zone"):
        net = find_child(z, "net_name")
        name = find_child(z, "name")
        cp = find_child(z, "connect_pads")
        cl = cp and find_child(cp, "clearance")
        out.append((name[1] if name else "", net[1] if net else "", cl[1] if cl else None))
    return out
