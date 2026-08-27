#!/usr/bin/env python3
"""Independent (coarse) result checker for Specctra DSN -> SES routing runs.

Parses the input .dsn and the output .ses with a small tolerant S-expression
parser and computes, without trusting either router's self-reported numbers:

  * number of nets declared in the DSN ``(network ...)`` section
    (total, and the subset with >= 2 pins, i.e. actually routable),
  * number of nets in the SES ``(network_out ...)`` section that contain at
    least one wire or via (coarse "has some routing" signal),
  * total wire length (sum of polyline segment lengths, converted to mm using
    the ``(resolution <unit> <value>)`` declared in the SES routes section),
  * via count.

CAVEAT: this is a *coarse completion metric only*. It does NOT verify
electrical connectivity (a net with one stray wire stub counts as "routed"),
nor clearance/DRC correctness. A proper Rust connectivity + clearance checker
comes later; until then treat these numbers as a sanity cross-check against
the tools' self-reports, not as ground truth.

Parser tolerances (learned from the upstream fixture suite):
  * ``(string_quote ")`` — the quote char appears as a literal, unbalanced
    ``"``. These directives are stripped before tokenizing.
  * Unterminated lists at EOF are closed silently; stray ``)`` are ignored.
  * Atoms may be bare or double-quoted; keyword matching is case-insensitive.

Usage:  python3 check_ses.py <input.dsn> <output.ses>   (prints JSON)
"""

from __future__ import annotations

import json
import math
import re
import sys

# --------------------------------------------------------------------------
# Tolerant S-expression parsing
# --------------------------------------------------------------------------

_STRING_QUOTE_RE = re.compile(r"\(\s*string_quote[^()]*\)", re.IGNORECASE)


def tokenize(text: str):
    """Yield tokens: '(' , ')' or atom strings (quotes already removed)."""
    # Remove pathological (string_quote ") directives (unbalanced quote char).
    text = _STRING_QUOTE_RE.sub("", text)
    i, n = 0, len(text)
    while i < n:
        c = text[i]
        if c in "()":
            yield c
            i += 1
        elif c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 1
            yield text[i + 1 : j]
            i = j + 1  # skip closing quote (or EOF)
        elif c.isspace():
            i += 1
        else:
            j = i
            while j < n and not text[j].isspace() and text[j] not in '()"':
                j += 1
            yield text[i:j]
            i = j


def parse_sexp(text: str):
    """Parse into nested lists of str atoms. Returns a synthetic root list."""
    root: list = []
    stack = [root]
    for tok in tokenize(text):
        if tok == "(":
            node: list = []
            stack[-1].append(node)
            stack.append(node)
        elif tok == ")":
            if len(stack) > 1:
                stack.pop()
            # else: stray ')', ignore
        else:
            stack[-1].append(tok)
    # Unterminated lists at EOF: implicitly closed (stack just abandoned).
    return root


def iter_nodes(node, name):
    """Recursively yield sub-lists whose head atom equals `name` (case-insens.)."""
    if isinstance(node, list):
        if node and isinstance(node[0], str) and node[0].lower() == name:
            yield node
        for child in node:
            yield from iter_nodes(child, name)


def child_nodes(node, name):
    """Direct child sub-lists with the given head atom."""
    for child in node:
        if (
            isinstance(child, list)
            and child
            and isinstance(child[0], str)
            and child[0].lower() == name
        ):
            yield child


def first_atom(node):
    """First plain-string element after the head, or None."""
    for el in node[1:]:
        if isinstance(el, str):
            return el
    return None


# --------------------------------------------------------------------------
# DSN side: declared nets
# --------------------------------------------------------------------------


def analyze_dsn(dsn_path: str) -> dict:
    with open(dsn_path, "r", encoding="utf-8", errors="replace") as f:
        tree = parse_sexp(f.read())

    nets_total = 0
    nets_multi_pin = 0
    pin_counts = {}
    for network in iter_nodes(tree, "network"):
        for net in child_nodes(network, "net"):
            name = first_atom(net)
            if name is None:
                continue
            pins = 0
            for pins_node in child_nodes(net, "pins"):
                pins += sum(1 for el in pins_node[1:] if isinstance(el, str))
            # A net may be declared more than once; merge pin counts.
            pin_counts[name] = pin_counts.get(name, 0) + pins
    nets_total = len(pin_counts)
    nets_multi_pin = sum(1 for c in pin_counts.values() if c >= 2)
    return {
        "dsn_nets_total": nets_total,
        "dsn_nets_multi_pin": nets_multi_pin,
        "dsn_net_names": pin_counts,  # name -> pin count (stripped in summary)
    }


# --------------------------------------------------------------------------
# SES side: what actually got routed
# --------------------------------------------------------------------------

_UNIT_MM = {"um": 0.001, "mm": 1.0, "cm": 10.0, "mil": 0.0254, "inch": 25.4}


def _resolution_mm(routes_scope) -> float:
    """mm per raw coordinate unit, from (resolution <unit> <value>)."""
    for res in iter_nodes(routes_scope, "resolution"):
        atoms = [el for el in res[1:] if isinstance(el, str)]
        if len(atoms) >= 2:
            unit = atoms[0].lower()
            try:
                per_unit = float(atoms[1])
            except ValueError:
                continue
            if per_unit > 0 and unit in _UNIT_MM:
                return _UNIT_MM[unit] / per_unit
    return float("nan")  # unknown resolution; lengths stay in raw units


def _path_length(path_node) -> float:
    """(path <layer> <width> x1 y1 x2 y2 ...) -> polyline length (raw units)."""
    nums = []
    for el in path_node[1:]:
        if isinstance(el, str):
            try:
                nums.append(float(el))
            except ValueError:
                pass  # layer name, aperture keywords, etc.
    # nums[0] is the width; points start at index 1.
    pts = nums[1:]
    length = 0.0
    for k in range(2, len(pts) - 1, 2):
        length += math.hypot(pts[k] - pts[k - 2], pts[k + 1] - pts[k - 1])
    return length


def analyze_ses(ses_path: str) -> dict:
    with open(ses_path, "r", encoding="utf-8", errors="replace") as f:
        tree = parse_sexp(f.read())

    routed_nets = set()
    present_nets = set()
    total_wire_len_raw = 0.0
    via_count = 0
    mm_per_unit = float("nan")

    for routes in iter_nodes(tree, "routes"):
        r = _resolution_mm(routes)
        if not math.isnan(r):
            mm_per_unit = r

    for netout in iter_nodes(tree, "network_out"):
        for net in child_nodes(netout, "net"):
            name = first_atom(net)
            if name is None:
                continue
            present_nets.add(name)
            wires = list(iter_nodes(net, "wire"))
            vias = list(iter_nodes(net, "via"))
            if wires or vias:
                routed_nets.add(name)
            via_count += len(vias)
            for wire in wires:
                for path in iter_nodes(wire, "path"):
                    total_wire_len_raw += _path_length(path)

    wire_len_mm = (
        total_wire_len_raw * mm_per_unit if not math.isnan(mm_per_unit) else None
    )
    return {
        "ses_nets_present": len(present_nets),
        "ses_nets_routed": len(routed_nets),
        "ses_routed_net_names": routed_nets,
        "ses_via_count": via_count,
        "ses_wire_length_mm": wire_len_mm,
        "ses_wire_length_raw": total_wire_len_raw,
    }


def check(dsn_path: str, ses_path: str) -> dict:
    """Combined coarse check; safe to call from run_bench.py."""
    d = analyze_dsn(dsn_path)
    s = analyze_ses(ses_path)
    routed_declared = len(
        s["ses_routed_net_names"] & set(d["dsn_net_names"].keys())
    )
    return {
        "dsn_nets_total": d["dsn_nets_total"],
        "dsn_nets_multi_pin": d["dsn_nets_multi_pin"],
        "ses_nets_present": s["ses_nets_present"],
        "ses_nets_routed": s["ses_nets_routed"],
        "ses_nets_routed_and_declared": routed_declared,
        "via_count": s["ses_via_count"],
        "wire_length_mm": (
            round(s["ses_wire_length_mm"], 3)
            if s["ses_wire_length_mm"] is not None
            else None
        ),
    }


def main(argv):
    if len(argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    print(json.dumps(check(argv[1], argv[2]), indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
