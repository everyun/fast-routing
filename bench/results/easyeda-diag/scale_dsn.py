#!/usr/bin/env python3
"""Scale a Specctra DSN's geometry by a constant factor (diagnostic experiment).

Coordinates, widths and clearances are multiplied; the resolution scope and
every name atom are left alone. Quoted atoms are never touched. The point is
to change the board's size *in board units* without changing anything else, to
test whether a router constant is calibrated in absolute board units.
"""
import re
import sys

SCALE = float(sys.argv[2]) if len(sys.argv) > 2 else 40.0

NUM_RE = re.compile(r"^[+-]?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?$")


def tokenize(text):
    i, n = 0, len(text)
    while i < n:
        c = text[i]
        if c.isspace():
            i += 1
        elif c in "()":
            yield c
            i += 1
        elif c == '"':
            j = text.index('"', i + 1)
            yield ("quoted", text[i + 1 : j])
            i = j + 1
        else:
            j = i
            while j < n and not text[j].isspace() and text[j] not in "()":
                j += 1
            yield ("atom", text[i:j])
            i = j


def parse(text):
    root = []
    stack = [root]
    for tok in tokenize(text):
        if tok == "(":
            node = []
            stack[-1].append(node)
            stack.append(node)
        elif tok == ")":
            if len(stack) > 1:
                stack.pop()
        else:
            stack[-1].append(tok)
    return root


def is_num(atom):
    return isinstance(atom, tuple) and atom[0] == "atom" and bool(NUM_RE.match(atom[1]))


def scale_atom(atom):
    v = float(atom[1]) * SCALE
    return ("atom", repr(v))


# args to scale, by scope head (0-based index including the head atom).
# circle: (circle layer d x y)  polygon: (polygon layer aperture x y...)
# path: (path layer width x y...)  rect/qarc: (rect layer x1 y1 x2 y2)
GEOM_FROM = {"polygon": 3, "path": 2, "rect": 2, "qarc": 2, "circle": 2, "vertex": 1}
FIXED_ARGS = {"pin": {3, 4}, "place": {2, 3}, "via": {2, 3}, "clear": {1}, "width": {1}, "grid": {2}}
NEVER = {"resolution", "layer", "net", "pins", "component", "image", "padstack",
         "class", "via_rule", "rule", "parser", "structure", "library", "network",
         "placement", "wiring", "pcb", "type", "directory", "topology", "boundary",
         "keepout", "place_keepout", "via_keepout", "wire_keepout", "shape",
         "plane", "contact", "front", "back", "off", "on", "signal", "power"}


def transform(node, head=None):
    if not isinstance(node, list) or not node:
        return node
    h = node[0][1] if isinstance(node[0], tuple) else None
    if h in NEVER and h != "boundary":
        # recurse into children but do not scale direct numeric args
        for k in range(1, len(node)):
            if isinstance(node[k], list):
                transform(node[k], h)
        return node
    for k in range(1, len(node)):
        child = node[k]
        if isinstance(child, list):
            transform(child, h)
        elif is_num(child):
            if h in GEOM_FROM and k >= GEOM_FROM[h]:
                node[k] = scale_atom(child)
            elif h in FIXED_ARGS and k in FIXED_ARGS[h]:
                node[k] = scale_atom(child)
    return node


def emit(node, out, depth=0):
    if isinstance(node, list):
        out.append("  " * depth + "(" + emit_inline(node, depth))
    else:
        raise AssertionError


def emit_inline(node, depth):
    parts = []
    for child in node:
        if isinstance(child, list):
            parts.append("(" + emit_inline(child, depth + 1) + ")")
        elif child[0] == "quoted":
            parts.append('"' + child[1] + '"')
        else:
            parts.append(child[1])
    return " ".join(parts)


def main():
    src = open(sys.argv[1]).read()
    tree = parse(src)
    for top in tree:
        transform(top)
    chunks = []
    for top in tree:
        chunks.append("(" + emit_inline(top, 0) + ")")
    open(sys.argv[3], "w").write("\n".join(chunks) + "\n")


if __name__ == "__main__":
    main()
