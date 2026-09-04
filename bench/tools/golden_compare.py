#!/usr/bin/env python3
"""G1.0/G1.1 golden-dump comparator: Java's resolved board model vs ours.

`bench/tools/java_dump/` (DumpDsn) makes the release jar import a board and write
it back out through `DsnWriter` — the headless `-do f.import.dsn` recipe cannot,
the v2.3.0 headless pipeline writes SES only. The result
(`bench/golden/dumps/<fixture>.import.dsn`) is not the input file — it is Java's
**resolved board model**, serialised: clearance classes materialised, placements
resolved, net classes bound, wiring re-imported with fixed states. That is the
G1.0 oracle, and `docs/ROADMAP.md` G1.1 gates our importer against it on a
compared-field list.

This module does three things, all in pure stdlib Python — nothing here depends
on the Rust tree, so the harness lands before the importer does:

1. **Parse** a Java dump into the compared-field list (`parse_java_dump`),
   reusing `bench/check_ses.py`'s tolerant S-expression machinery.
2. **Define** the JSON exchange format the Rust side will emit
   (`SCHEMA_ID`; the normative description lives in `bench/tools/README.md`),
   and adapt a Java dump *into* that format, so one comparator serves both
   sides and the Java-vs-Java self-comparison exercises every code path.
3. **Compare** two documents field group by field group, normalising away the
   known-lossy axes — class-pair spelling, float formatting and ordering, plus
   the four the dump format itself forces (see `EXCLUSIONS`, or `--exclusions`)
   — and reporting, field by field, what is left.

Usage:

    python3 bench/tools/golden_compare.py --java-dump J --ours O [--json]
    python3 bench/tools/golden_compare.py --all [--ours-dir DIR]
    python3 bench/tools/golden_compare.py --emit <dump>      # reference encoder
    python3 bench/tools/golden_compare.py --exclusions
    python3 bench/tools/golden_compare.py --self-test

`--ours` accepts either a schema JSON document or another DSN dump (it is
adapted on the fly), which is what makes the self-comparison possible.

Exit codes: 0 = every compared group agrees, 1 = at least one difference,
2 = usage / IO error.
"""

from __future__ import annotations

import argparse
import glob
import json
import math
import os
import re
import sys
from dataclasses import dataclass, field
from typing import Any, Iterable, Sequence

# `bench/check_ses.py` already owns the tolerant Specctra tokenizer (quoted-atom
# handling, `(string_quote ")` stripping, unterminated-list recovery) and it is
# exercised by the referee harness on every bench run. Import it rather than
# growing a second, subtly-different lexer.
_BENCH_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
if _BENCH_DIR not in sys.path:
    sys.path.insert(0, _BENCH_DIR)

from check_ses import (  # noqa: E402  (path bootstrap must precede the import)
    _STRING_QUOTE_RE,
    child_nodes,
    parse_sexp,
)

SCHEMA_ID = "fast-routing.golden-dump/1"

DEFAULT_DUMP_DIR = os.path.join(_BENCH_DIR, "golden", "dumps")
DEFAULT_MANIFEST = os.path.join(_BENCH_DIR, "fixtures-manifest.json")

# Quantisation applied to every number before comparison. DSN dumps carry
# coordinates in the design's own unit at a declared resolution (typically
# `um 10`, i.e. 0.1-unit granularity), so six decimals is orders of magnitude
# below any semantic difference and orders of magnitude above double
# print/parse noise.
DEFAULT_DECIMALS = 6


# ==========================================================================
# Known-lossy exclusions
# ==========================================================================
# Every entry is a place where the Java dump cannot distinguish two board
# models that differ, or where it distinguishes two board models that do not.
# A comparator that ignores these is *not* weaker than the oracle — it is
# exactly as strong as the oracle, and honest about it.

EXCLUSIONS: list[tuple[str, str]] = [
    (
        "class-pair spelling",
        "The writer emits a clearance-matrix diagonal cell as `(type smd)` "
        "(`Rule.write_named_clearance_rules`), never `(type smd_smd)`, while the "
        "reader accepts `a_b`, `a-b` and `\"a\"_b` for the same pair "
        "(dsn-board-semantics.md §7.3). Every `(type ...)` token is normalised to "
        "one unordered class pair, so all three spellings agree. The four cases, "
        "in order: `smd_to_turn_gap` is not a matrix entry at all; an unescaped "
        "`-` is `nextStringList`'s separator, giving the pair; a *quoted* token is "
        "one class name, because `IdentifierType` reserves `_` and `-` and quotes "
        "any name containing them (so `\"board_edge\"` stays one class); a *bare* "
        "token containing `_` cannot be a name this writer produced, so it splits "
        "on the first `_` the way `Structure.setClearanceRule` does. The JSON "
        "schema sidesteps the whole ambiguity with an explicit two-element "
        "`classes` array.",
    ),
    (
        "clearance-matrix off-diagonal",
        "`Rule.write_non_default_clearance_rules` is commented out in the release "
        "revision (`parser/Rule.java:185`), so the dump carries only the matrix "
        "diagonal plus `(default, default)`. Off-diagonal cells are *not "
        "observable* through this oracle; both sides are projected onto the "
        "observable set and the number of dropped entries is reported. "
        "Off-diagonal parity is gated by G1.2a (`java -drc`) instead.",
    ),
    (
        "float formatting",
        f"Java writes doubles with `String.valueOf`; we will not. Every number is "
        f"quantised to {DEFAULT_DECIMALS} decimals (`--float-decimals`) before "
        f"comparison, and `-0.0` is folded to `0.0`. Values that survive "
        f"quantisation but land within one quantum of each other are reported as "
        f"`float-noise` notes rather than differences.",
    ),
    (
        "ordering",
        "Collections whose DSN order reflects Java-internal iteration order "
        "(`UndoableObjects` traversal, `HashMap` walks, library insertion order) "
        "are compared as multisets or as name-keyed maps: net pin lists, net-class "
        "member lists, via infos, via rules, the structure `(via ...)` padstack "
        "list, wiring items, structure areas, padstack shapes. Order *is* kept "
        "where it is semantic: the layer list (layer index decides layer-flip and "
        "matrix indexing).",
    ),
    (
        "design name and provenance",
        "`(pcb <name>)` is the `-do` output file's stem, and the `(parser ...)` "
        "scope carries `host_cad`/`host_version`/`string_quote`/"
        "`generated_by_freeroute`. Neither is board model; both are recorded as "
        "metadata and never compared.",
    ),
    (
        "area geometry reduction",
        "Board outline, structure keepouts and planes are compared as "
        "`(kind, layer, net, clearance class, corner count, bounding box)` rather "
        "than corner-by-corner: those polygons run to thousands of corners and "
        "their exact corner *set* is an input echo, while their extent is the "
        "thing the router sees. Padstack shapes, which are small and decide "
        "clearance, are compared exactly.",
    ),
    (
        "component rotation",
        "`Component.write_scope` writes the placement angle as "
        "`(int) Math.round(p_component.get_rotation_in_degree())` "
        "(`parser/Component.java:67`) — an *integer* degree, while the board model "
        "holds a double. A footprint placed at 352.5deg is written as `353`, so "
        "the oracle cannot see anything below one degree. Both sides are put "
        "through the same `Math.round` before comparison; sub-degree placement "
        "parity is not observable here.",
    ),
    (
        "conduction-area fixed state",
        "`Wiring.write_conduction_area_scope` (`parser/Wiring.java:148-180`) never "
        "calls `write_fixed_state`, so every conduction area reads back as "
        "`unfixed` no matter what it was inserted with -- and planes are inserted "
        "`SYSTEM_FIXED` (`Structure.java:967`). The `wiring` group therefore folds "
        "`conduction_area` to a single `unfixed` bucket on both sides. Conduction "
        "areas on *power* layers are not in the wiring scope at all: "
        "`Wiring.write_scope` skips them and `Structure.write_conduction_areas` "
        "emits them as `(plane ...)`, where `structure_areas` picks them up.",
    ),
    (
        "pre-normalisation trace splitting",
        "`(wire ...)` records in the dump are counted by kind and fixed state, not "
        "matched one-to-one: upstream re-imports wiring with "
        "`insert_trace_without_cleaning` and only then normalises "
        "(dsn-board-semantics.md §10.4), so the *split* of a net's copper into "
        "trace records is an artefact of insertion order. Counts by "
        "`(kind, fixed state)` are the invariant G1.1 names.",
    ),
]


# ==========================================================================
# Dump text canonicalisation
# ==========================================================================
# `check_ses.tokenize` ends a quoted atom at the first `"` that is followed by
# whitespace, a paren or EOF. That heuristic exists because KiCad exports
# unescaped inch marks inside quoted strings, and it is right for *input* files.
# It is wrong for a Java-written dump, where `-` and `_` are reserved characters
# (`WriteScopeParameter`), so `Net.write_pin` emits pin references as
# `"COMP"-"PIN"` — a closing quote followed by `-`. The tolerant scan runs past
# it and swallows the rest of the file.
#
# Rather than fork the tokenizer, canonicalise the text first. `IdentifierType`
# strips the string-quote character out of every name it writes, so in a dump
# quotes strictly alternate: open, close, open, close. That makes an exact
# re-encoding possible — replace each quoted region with a marker-prefixed bare
# atom whose delimiter-significant characters are escaped, after which the text
# contains no `"` at all and the tolerant path is exact.

_MARK = "\x02"  # prefix: this atom (or atom fragment) came from a quoted string
_ESC = "\x01"  # escape introducer, followed by two lowercase hex digits
_UNSAFE = set(_MARK + _ESC + '"()-' + " \t\n\r\f\v")


class DumpParseError(Exception):
    """The dump is not a Specctra DSN this parser can read."""


def _escape(text: str) -> str:
    return "".join(
        f"{_ESC}{ord(ch):02x}" if ch in _UNSAFE else ch for ch in text
    )


_UNESCAPE_RE = re.compile(_ESC + "([0-9a-f]{2})")


def _unescape(text: str) -> str:
    return _UNESCAPE_RE.sub(lambda m: chr(int(m.group(1), 16)), text)


def canonicalise_dump_text(text: str) -> str:
    """Re-encode quoted atoms so `check_ses.tokenize` cannot mis-scan them.

    Raises `DumpParseError` if the file does not use `"` as its string quote
    (every Freerouting dump does; `Parser.write_scope` echoes the quote it read)
    or if it already contains the marker characters.
    """
    if _MARK in text or _ESC in text:
        raise DumpParseError("dump contains U+0001/U+0002; cannot canonicalise")
    quote_decl = re.search(r"\(\s*string_quote\s*(\S)\s*\)", text)
    if quote_decl and quote_decl.group(1) != '"':
        raise DumpParseError(
            f"unsupported string_quote {quote_decl.group(1)!r}; only '\"' is handled"
        )
    # Strip `(string_quote ")` exactly the way check_ses does, so the lone quote
    # inside it does not desynchronise the alternation below (and so the second
    # application inside tokenize() is a no-op).
    text = _STRING_QUOTE_RE.sub("", text)

    out: list[str] = []
    i = 0
    while True:
        start = text.find('"', i)
        if start < 0:
            out.append(text[i:])
            break
        end = text.find('"', start + 1)
        out.append(text[i:start])
        if end < 0:  # unterminated final quote — take the rest verbatim
            out.append(_MARK + _escape(text[start + 1 :]))
            break
        out.append(_MARK + _escape(text[start + 1 : end]))
        i = end + 1
    return "".join(out)


def decode(atom: str) -> str:
    """Turn a canonicalised token back into the identifier Java wrote."""
    return _unescape(atom.replace(_MARK, ""))


def split_ref(atom: str) -> tuple[str, str] | None:
    """Split a `<component>-<pin>` reference written by `Net.write_pin`.

    Both halves went through `IdentifierType.write`, which quotes anything
    containing `-`, so after canonicalisation the *only* unescaped `-` in the
    token is the separator. Returns None when there is not exactly one.
    """
    parts = atom.split("-")
    if len(parts) != 2:
        return None
    return decode(parts[0]), decode(parts[1])


def atoms(node: list) -> list[str]:
    """Decoded string elements of a scope, excluding the head keyword."""
    return [decode(el) for el in node[1:] if isinstance(el, str)]


def raw_atoms(node: list) -> list[str]:
    """Still-encoded string elements of a scope, excluding the head keyword."""
    return [el for el in node[1:] if isinstance(el, str)]


def first_child(node: list, name: str) -> list | None:
    for child in child_nodes(node, name):
        return child
    return None


def _num(text: str) -> float | None:
    try:
        return float(text)
    except (TypeError, ValueError):
        return None


def _nums(items: Iterable[str]) -> list[float]:
    out: list[float] = []
    for it in items:
        value = _num(it)
        if value is not None:
            out.append(value)
    return out


# ==========================================================================
# Shape extraction
# ==========================================================================


def _shape_node(node: list) -> list | None:
    """First sub-list of a scope that looks like a geometric shape."""
    for child in node:
        if isinstance(child, list) and child and isinstance(child[0], str):
            if child[0].lower() in _SHAPE_KINDS:
                return child
    return None


_SHAPE_KINDS = {"circle", "rect", "polygon", "path", "polyline_path", "polygon_path"}


def read_shape(node: list) -> dict:
    """`(circle L d [x y])` / `(rect L x1 y1 x2 y2)` / `(polygon L ap x y …)` → dict.

    `coords` keeps every number after the layer name, in file order, including
    the polygon aperture width and the path width — those are part of the shape
    as the reader sees them.
    """
    kind = node[0].lower()
    items = raw_atoms(node)
    layer = decode(items[0]) if items else ""
    coords = _nums(items[1:])
    return {"kind": kind, "layer": layer, "coords": coords}


def _bbox(kind: str, coords: Sequence[float]) -> list[float] | None:
    """Axis-aligned extent of a shape, for the reduced area comparison."""
    if kind == "circle" and len(coords) >= 1:
        diameter = coords[0]
        cx = coords[1] if len(coords) > 1 else 0.0
        cy = coords[2] if len(coords) > 2 else 0.0
        r = diameter / 2.0
        return [cx - r, cy - r, cx + r, cy + r]
    if kind == "rect" and len(coords) >= 4:
        xs = [coords[0], coords[2]]
        ys = [coords[1], coords[3]]
        return [min(xs), min(ys), max(xs), max(ys)]
    if kind in ("polygon", "path", "polygon_path"):
        pts = coords[1:]  # drop the aperture / width
    elif kind == "polyline_path":
        pts = coords[1:]
    else:
        pts = list(coords)
    if len(pts) < 2:
        return None
    xs = pts[0::2]
    ys = pts[1::2]
    return [min(xs), min(ys), max(xs), max(ys)]


def _corner_count(kind: str, coords: Sequence[float]) -> int:
    if kind == "circle":
        return 0
    if kind == "rect":
        return 4
    if kind == "polyline_path":
        return max(0, (len(coords) - 1) // 4)
    return max(0, (len(coords) - 1) // 2)


def reduce_area(node: list, kind_override: str | None = None) -> dict:
    """Area scope → the reduced signature described in `EXCLUSIONS`."""
    shape = _shape_node(node)
    out: dict[str, Any] = {"kind": kind_override or node[0].lower()}
    if shape is None:
        out.update({"shape": None, "layer": None, "corners": 0, "bbox": None})
    else:
        s = read_shape(shape)
        out.update(
            {
                "shape": s["kind"],
                "layer": s["layer"],
                "corners": _corner_count(s["kind"], s["coords"]),
                "bbox": _bbox(s["kind"], s["coords"]),
            }
        )
    out["windows"] = sum(1 for _ in child_nodes(node, "window"))
    cc = first_child(node, "clearance_class")
    out["clearance_class"] = atoms(cc)[0] if cc and atoms(cc) else None
    return out


# ==========================================================================
# Rule scopes
# ==========================================================================

_SMD_TO_TURN_GAP = "smd_to_turn_gap"


def read_clearance_type(node: list) -> tuple[str, ...] | None:
    """`(type …)` → the normalised class pair, or None for `smd_to_turn_gap`.

    See the "class-pair spelling" exclusion. The token is examined *before*
    decoding, because quoting and escaping are what disambiguate the four forms
    the reader accepts (dsn-board-semantics.md §7.3):

    * `smd_to_turn_gap` — not a matrix entry; hard-coded bare by the writer.
    * `a-b`  — an unescaped `-` is `nextStringList('-')`'s separator → pair.
    * `"X"`  — quoted, so `X` may contain `_`/`-`/spaces → the diagonal `(X, X)`.
      `IdentifierType` treats `_` and `-` as reserved, so the writer quotes any
      class name containing one; a quoted token is therefore always one name.
    * `a_b`  — *bare* and containing `_`, so it cannot be a name the writer
      produced → split on the first `_`, matching `Structure.setClearanceRule`.
      `smd_smd` and `smd` thus both normalise to `("smd", "smd")`.
    """
    items = raw_atoms(node)
    if not items:
        return ()
    if len(items) == 1:
        token = items[0]
        if decode(token).lower() == _SMD_TO_TURN_GAP:
            return None
        pair = split_ref(token)
        if pair is not None:
            return tuple(sorted(pair))
        if token.startswith(_MARK):
            name = decode(token)
            return (name, name)
        name = decode(token)
        if "_" in name:
            left, right = name.split("_", 1)
            return tuple(sorted((left, right)))
        return (name, name)
    # `(type a - b)` with whitespace around the separator: nextStringList('-')
    # would produce the same two names. Never emitted by the writer.
    names = [decode(t) for t in items if t != "-"]
    if len(names) >= 2:
        return tuple(sorted(names[:2]))
    return (names[0], names[0]) if names else ()


@dataclass
class RuleScope:
    """One `(rule (width …) (clearance …)*)` block."""

    width: float | None = None
    smd_to_turn_gap: float | None = None
    default_clearance: float | None = None
    pairs: dict[tuple[str, ...], float] = field(default_factory=dict)

    @property
    def has_clearances(self) -> bool:
        return (
            self.default_clearance is not None
            or self.smd_to_turn_gap is not None
            or bool(self.pairs)
        )


def read_rule_scope(node: list) -> RuleScope:
    rule = RuleScope()
    width = first_child(node, "width")
    if width is not None:
        rule.width = _num(atoms(width)[0]) if atoms(width) else None
    for clearance in child_nodes(node, "clearance"):
        values = _nums(raw_atoms(clearance))
        if not values:
            continue
        value = values[0]
        type_node = first_child(clearance, "type")
        if type_node is None:
            rule.default_clearance = value
            continue
        pair = read_clearance_type(type_node)
        if pair is None:
            rule.smd_to_turn_gap = value
        elif pair:
            rule.pairs[pair] = value
    return rule


# ==========================================================================
# Java dump → schema document
# ==========================================================================

_FIXED_SPELLING = {
    # `Wiring.write_fixed_state`; the `else` branch is USER_FIXED.
    "fix": "system_fixed",
    "shove_fixed": "shove_fixed",
    "protect": "user_fixed",
}

_TRACE_SHAPES = {"path", "polyline_path", "polygon_path"}


def _fixed_state(node: list) -> str:
    type_node = first_child(node, "type")
    if type_node is None:
        return "unfixed"
    names = atoms(type_node)
    if not names:
        return "unfixed"
    return _FIXED_SPELLING.get(names[0].lower(), names[0].lower())


def parse_java_dump(path: str) -> dict:
    """Read a `*.import.dsn` golden dump into the exchange schema."""
    with open(path, "r", encoding="utf-8", errors="replace") as handle:
        text = handle.read()
    return adapt_java_dump(text, source=path)


def adapt_java_dump(text: str, source: str = "<memory>") -> dict:
    tree = parse_sexp(canonicalise_dump_text(text))
    pcb = None
    for node in tree:
        if isinstance(node, list) and node and str(node[0]).lower() == "pcb":
            pcb = node
            break
    if pcb is None:
        raise DumpParseError(f"{source}: no (pcb …) scope")

    structure = first_child(pcb, "structure")
    placement = first_child(pcb, "placement")
    library = first_child(pcb, "library")
    network = first_child(pcb, "network")
    wiring = first_child(pcb, "wiring")
    if structure is None:
        raise DumpParseError(f"{source}: no (structure …) scope")

    doc: dict[str, Any] = {
        "schema": SCHEMA_ID,
        "producer": "java-dump-adapter",
        "source": os.path.basename(source),
        "design": (atoms(pcb) or [""])[0],
    }

    # --- resolution / unit -------------------------------------------------
    resolution = first_child(pcb, "resolution")
    unit = first_child(pcb, "unit")
    res_atoms = atoms(resolution) if resolution is not None else []
    doc["resolution"] = {
        "unit": res_atoms[0].lower() if res_atoms else None,
        "value": _num(res_atoms[1]) if len(res_atoms) > 1 else None,
    }
    doc["unit"] = (atoms(unit)[0].lower() if unit is not None and atoms(unit) else None)

    # --- layers ------------------------------------------------------------
    layers: list[dict] = []
    layer_rules: dict[str, RuleScope] = {}
    for node in child_nodes(structure, "layer"):
        names = atoms(node)
        name = names[0] if names else ""
        type_node = first_child(node, "type")
        layers.append(
            {
                "name": name,
                "type": (atoms(type_node)[0].lower() if type_node and atoms(type_node) else None),
            }
        )
        rule_node = first_child(node, "rule")
        if rule_node is not None:
            layer_rules[name] = read_rule_scope(rule_node)
    doc["layers"] = layers
    layer_names = [layer["name"] for layer in layers]

    # --- rules / clearance matrix -----------------------------------------
    board_rule_node = first_child(structure, "rule")
    board_rule = read_rule_scope(board_rule_node) if board_rule_node is not None else RuleScope()
    matrix: list[dict] = []
    widths: dict[str, float | None] = {}
    gaps: dict[str, float | None] = {}
    for name in layer_names:
        layer_rule = layer_rules.get(name)
        source_rule = layer_rule if (layer_rule and layer_rule.has_clearances) else board_rule
        widths[name] = (
            layer_rule.width if (layer_rule and layer_rule.width is not None) else board_rule.width
        )
        gaps[name] = source_rule.smd_to_turn_gap
        if source_rule.default_clearance is not None:
            matrix.append(
                {
                    "classes": ["default", "default"],
                    "layer": name,
                    "value": source_rule.default_clearance,
                }
            )
        for pair, value in source_rule.pairs.items():
            matrix.append({"classes": list(pair), "layer": name, "value": value})
    snap = first_child(structure, "snap_angle")
    control = first_child(structure, "control")
    via_at_smd = first_child(control, "via_at_smd") if control is not None else None
    doc["rules"] = {
        "clearance_matrix": matrix,
        "default_trace_width": widths,
        "smd_to_turn_gap": gaps,
        "snap_angle": (atoms(snap)[0].lower() if snap is not None and atoms(snap) else None),
        "via_at_smd": (
            atoms(via_at_smd)[0].lower()
            if via_at_smd is not None and atoms(via_at_smd)
            else None
        ),
    }

    # --- boundary ----------------------------------------------------------
    boundary: list[dict] = []
    for node in child_nodes(structure, "boundary"):
        boundary.append(reduce_area(node, kind_override="boundary"))
    doc["boundary"] = boundary

    # --- structure areas ---------------------------------------------------
    areas: list[dict] = []
    for kind in ("keepout", "via_keepout", "place_keepout"):
        for node in child_nodes(structure, kind):
            areas.append(reduce_area(node, kind_override=kind))
    for node in child_nodes(structure, "plane"):
        entry = reduce_area(node, kind_override="plane")
        names = atoms(node)
        entry["net"] = names[0] if names else None
        areas.append(entry)
    doc["structure_areas"] = areas

    # --- library: padstacks + images --------------------------------------
    padstacks: list[dict] = []
    images: list[dict] = []
    if library is not None:
        for node in child_nodes(library, "padstack"):
            names = atoms(node)
            attach = first_child(node, "attach")
            absolute = first_child(node, "absolute")
            shapes = []
            for shape_scope in child_nodes(node, "shape"):
                inner = _shape_node(shape_scope)
                if inner is not None:
                    shapes.append(read_shape(inner))
            padstacks.append(
                {
                    "name": names[0] if names else "",
                    # `Library.write_padstack_scope` only emits `(attach off)` /
                    # `(absolute on)`; the omitted case is the model default.
                    "attach": not (attach is not None and atoms(attach)[:1] == ["off"]),
                    "absolute": absolute is not None and atoms(absolute)[:1] == ["on"],
                    "shapes": shapes,
                }
            )
        for node in child_nodes(library, "image"):
            names = atoms(node)
            side = first_child(node, "side")
            pins = []
            for pin in child_nodes(node, "pin"):
                items = raw_atoms(pin)
                rotate = first_child(pin, "rotate")
                coords = _nums(items[2:])
                pins.append(
                    {
                        "padstack": decode(items[0]) if items else "",
                        "name": decode(items[1]) if len(items) > 1 else "",
                        "x": coords[0] if coords else None,
                        "y": coords[1] if len(coords) > 1 else None,
                        "rotation": (
                            int(_nums(raw_atoms(rotate))[0])
                            if rotate is not None and _nums(raw_atoms(rotate))
                            else 0
                        ),
                    }
                )
            outlines = [
                reduce_area(o, kind_override="outline") for o in child_nodes(node, "outline")
            ]
            images.append(
                {
                    "name": names[0] if names else "",
                    "side": (atoms(side)[0].lower() if side is not None and atoms(side) else None),
                    "pins": pins,
                    "keepouts": sum(1 for _ in child_nodes(node, "keepout")),
                    "via_keepouts": sum(1 for _ in child_nodes(node, "via_keepout")),
                    "place_keepouts": sum(1 for _ in child_nodes(node, "place_keepout")),
                    "outlines": outlines,
                }
            )
    doc["padstacks"] = padstacks
    doc["images"] = images

    # --- placement ---------------------------------------------------------
    components: list[dict] = []
    if placement is not None:
        for comp in child_nodes(placement, "component"):
            image_names = atoms(comp)
            image_name = image_names[0] if image_names else ""
            for place in child_nodes(comp, "place"):
                items = raw_atoms(place)
                entry: dict[str, Any] = {
                    "name": decode(items[0]) if items else "",
                    "image": image_name,
                    "position_fixed": first_child(place, "lock_type") is not None,
                }
                if len(items) >= 5:
                    entry.update(
                        {
                            "placed": True,
                            "x": _num(items[1]),
                            "y": _num(items[2]),
                            "side": decode(items[3]).lower(),
                            "rotation": _num(items[4]),
                        }
                    )
                else:
                    entry.update(
                        {"placed": False, "x": None, "y": None, "side": None, "rotation": None}
                    )
                entry["pins"] = [
                    {
                        "name": (atoms(pin)[0] if atoms(pin) else ""),
                        "clearance_class": _clearance_class_of(pin),
                    }
                    for pin in child_nodes(place, "pin")
                ]
                keepouts = []
                for kind in ("keepout", "via_keepout", "place_keepout"):
                    for node in child_nodes(place, kind):
                        keepouts.append(
                            {
                                "kind": kind,
                                "name": (atoms(node)[0] if atoms(node) else ""),
                                "clearance_class": _clearance_class_of(node),
                            }
                        )
                entry["keepouts"] = keepouts
                components.append(entry)
    doc["components"] = components

    # --- network -----------------------------------------------------------
    nets: list[dict] = []
    net_classes: list[dict] = []
    via_infos: list[dict] = []
    via_rules: list[dict] = []
    if network is not None:
        for node in child_nodes(network, "net"):
            items = raw_atoms(node)
            pins: list[list[str]] = []
            pins_node = first_child(node, "pins")
            if pins_node is not None:
                for token in raw_atoms(pins_node):
                    ref = split_ref(token)
                    pins.append(list(ref) if ref else [decode(token), ""])
            nets.append(
                {
                    "name": decode(items[0]) if items else "",
                    "subnet": int(_num(items[1]) or 0) if len(items) > 1 else 0,
                    "pins": pins,
                }
            )
        for node in child_nodes(network, "via"):
            items = atoms(node)
            via_infos.append(
                {
                    "name": items[0] if items else "",
                    "padstack": items[1] if len(items) > 1 else "",
                    "clearance_class": items[2] if len(items) > 2 else "",
                    "attach": len(items) > 3 and items[3].lower() == "attach",
                }
            )
        for node in child_nodes(network, "via_rule"):
            items = atoms(node)
            via_rules.append({"name": items[0] if items else "", "vias": items[1:]})
        for node in child_nodes(network, "class"):
            items = atoms(node)
            rule_node = first_child(node, "rule")
            rule = read_rule_scope(rule_node) if rule_node is not None else RuleScope()
            circuit = first_child(node, "circuit")
            use_layer: list[str] = []
            length: list[float] = []
            if circuit is not None:
                ul = first_child(circuit, "use_layer")
                if ul is not None:
                    use_layer = atoms(ul)
                ln = first_child(circuit, "length")
                if ln is not None:
                    length = _nums(raw_atoms(ln))
            via_rule = first_child(node, "via_rule")
            pull_tight = first_child(node, "pull_tight")
            shove_fixed = first_child(node, "shove_fixed")
            net_classes.append(
                {
                    "name": items[0] if items else "",
                    "nets": items[1:],
                    "clearance_class": _clearance_class_of(node),
                    "via_rule": (
                        atoms(via_rule)[0] if via_rule is not None and atoms(via_rule) else None
                    ),
                    "trace_width": rule.width,
                    "use_layer": use_layer,
                    "length_max": length[0] if length else None,
                    "length_min": length[1] if len(length) > 1 else None,
                    # `Network.write_net_class` only emits the non-default value.
                    "pull_tight": not (
                        pull_tight is not None and atoms(pull_tight)[:1] == ["off"]
                    ),
                    "shove_fixed": shove_fixed is not None
                    and atoms(shove_fixed)[:1] == ["on"],
                }
            )
    doc["nets"] = nets
    doc["net_classes"] = net_classes
    doc["via_infos"] = via_infos
    doc["via_rules"] = via_rules
    # `(via …)` means two different things: the structure scope's routing-via
    # padstack list (`Structure.write_via_padstacks`, multiplicity is real) and
    # the network scope's via infos, read above.
    structure_vias = first_child(structure, "via")
    doc["via_padstacks"] = atoms(structure_vias) if structure_vias is not None else []

    # --- wiring ------------------------------------------------------------
    counts: dict[tuple[str, str], int] = {}
    if wiring is not None:
        for node in child_nodes(wiring, "wire"):
            shape = _shape_node(node)
            kind = (
                "trace"
                if shape is not None and shape[0].lower() in _TRACE_SHAPES
                else "conduction_area"
            )
            key = (kind, _fixed_state(node))
            counts[key] = counts.get(key, 0) + 1
        for node in child_nodes(wiring, "via"):
            key = ("via", _fixed_state(node))
            counts[key] = counts.get(key, 0) + 1
    doc["wiring"] = {
        "by_kind_fixed": [
            {"kind": kind, "fixed": fixed, "count": count}
            for (kind, fixed), count in sorted(counts.items())
        ]
    }
    return doc


def _clearance_class_of(node: list) -> str | None:
    cc = first_child(node, "clearance_class")
    if cc is None:
        return None
    names = atoms(cc)
    return names[0] if names else None


# ==========================================================================
# Normalisation and comparison
# ==========================================================================


def _canon(value: Any, decimals: int) -> Any:
    """Quantise numbers, recursively; fold `-0.0` to `0.0` and int to float.

    `90` from a JSON emitter and `90.0` from the DSN reader are the same
    rotation, so ints and floats are unified before comparison. `bool` is
    excluded — it is an `int` subclass in Python and `True == 1` must not make
    `attach: true` compare equal to `attach: 1`.
    """
    if isinstance(value, bool):
        return value
    if isinstance(value, (int, float)):
        return round(float(value), decimals) + 0.0
    if isinstance(value, (list, tuple)):
        return [_canon(v, decimals) for v in value]
    if isinstance(value, dict):
        return {k: _canon(v, decimals) for k, v in sorted(value.items())}
    return value


def _key(value: Any, decimals: int) -> str:
    """Stable, order-free string key for multiset comparison."""
    return json.dumps(_canon(value, decimals), sort_keys=True, ensure_ascii=False)


_NUM = object()  # placeholder standing in for any number in a skeleton


def _skeleton(value: Any) -> Any:
    """The value with every number replaced by a placeholder.

    Two values whose skeletons are equal differ *only* in their numbers, which
    is the precondition for calling a difference float noise. Without this,
    `{"side": "back"}` vs `{"side": "front"}` would look like "no numeric leaf
    disagrees" and be waved through.
    """
    if isinstance(value, bool):
        return value
    if isinstance(value, (int, float)):
        return _NUM
    if isinstance(value, (list, tuple)):
        return [_skeleton(v) for v in value]
    if isinstance(value, dict):
        return {k: _skeleton(v) for k, v in sorted(value.items())}
    return value


def _numeric_pairs(left: Any, right: Any) -> list[tuple[float, float]]:
    """Every aligned (left, right) numeric leaf. Call only on equal skeletons."""
    if isinstance(left, bool) or isinstance(right, bool):
        return []
    if isinstance(left, (int, float)) and isinstance(right, (int, float)):
        return [(float(left), float(right))]
    if isinstance(left, (list, tuple)) and isinstance(right, (list, tuple)):
        out: list[tuple[float, float]] = []
        for a, b in zip(left, right):
            out.extend(_numeric_pairs(a, b))
        return out
    if isinstance(left, dict) and isinstance(right, dict):
        out = []
        for k in sorted(left.keys() & right.keys()):
            out.extend(_numeric_pairs(left[k], right[k]))
        return out
    return []


@dataclass
class Difference:
    group: str
    op: str  # "-" only-left, "+" only-right, "~" value differs
    key: str
    left: Any = None
    right: Any = None
    note: str | None = None

    def render(self) -> str:
        if self.op == "-":
            body = f"left={_short(self.left)}  right=<absent>"
        elif self.op == "+":
            body = f"left=<absent>  right={_short(self.right)}"
        else:
            body = f"left={_short(self.left)}  right={_short(self.right)}"
        tail = f"   [{self.note}]" if self.note else ""
        return f"  {self.op} {self.key}: {body}{tail}"


def _short(value: Any, limit: int = 110) -> str:
    text = json.dumps(value, sort_keys=True, ensure_ascii=False)
    return text if len(text) <= limit else text[: limit - 3] + "..."


@dataclass
class GroupResult:
    name: str
    gate: str  # "G1.1" or "extra"
    left_size: int
    right_size: int
    differences: list[Difference] = field(default_factory=list)
    notes: list[Difference] = field(default_factory=list)
    excluded: int = 0

    @property
    def ok(self) -> bool:
        return not self.differences


@dataclass
class CompareResult:
    fixture: str
    left_label: str
    right_label: str
    groups: list[GroupResult] = field(default_factory=list)
    errors: list[str] = field(default_factory=list)

    @property
    def ok(self) -> bool:
        return not self.errors and all(g.ok for g in self.groups)

    @property
    def diff_count(self) -> int:
        return sum(len(g.differences) for g in self.groups)

    def group(self, name: str) -> GroupResult | None:
        for g in self.groups:
            if g.name == name:
                return g
        return None


class _Differ:
    """Builds `GroupResult`s from the three shapes every group reduces to."""

    def __init__(self, decimals: int) -> None:
        self.decimals = decimals
        self.quantum = 10.0**-decimals

    def _float_noise(self, left: Any, right: Any) -> bool:
        """True when the two values differ only by sub-quantum numeric drift.

        Such a pair survived quantisation but landed within one quantum of the
        rounding boundary. It is reported as a note rather than a difference:
        calling it a parity failure would make the gate depend on where a
        decimal happened to fall.
        """
        if _skeleton(left) != _skeleton(right):
            return False
        pairs = _numeric_pairs(left, right)
        if not pairs:
            return False
        for a, b in pairs:
            if a != a or b != b:  # NaN
                return False
            if abs(a - b) > 1.5 * self.quantum + 1e-9 * max(abs(a), abs(b)):
                return False
        return True

    def mapping(
        self,
        name: str,
        gate: str,
        left: dict,
        right: dict,
        excluded: int = 0,
        depth: int = 2,
    ) -> GroupResult:
        """Compare two name-keyed maps.

        When both sides hold a dict under the same key, recurse (up to `depth`)
        and report `key.subkey` instead of dumping two whole records: the point
        of a field-level report is to say *which* pin's clearance class moved,
        not that the component "changed".
        """
        result = GroupResult(name, gate, len(left), len(right), excluded=excluded)
        for key in sorted(set(left) | set(right)):
            if key not in right:
                result.differences.append(Difference(name, "-", key, left=left[key]))
                continue
            if key not in left:
                result.differences.append(Difference(name, "+", key, right=right[key]))
                continue
            lv, rv = left[key], right[key]
            if _canon(lv, self.decimals) == _canon(rv, self.decimals):
                continue
            if depth > 0 and isinstance(lv, dict) and isinstance(rv, dict):
                sub = self.mapping(name, gate, lv, rv, depth=depth - 1)
                if sub.differences or sub.notes:
                    for diff in sub.differences:
                        diff.key = f"{key}.{diff.key}"
                        result.differences.append(diff)
                    for diff in sub.notes:
                        diff.key = f"{key}.{diff.key}"
                        result.notes.append(diff)
                    continue
            diff = Difference(name, "~", key, left=lv, right=rv)
            if self._float_noise(lv, rv):
                diff.note = "float-noise"
                result.notes.append(diff)
            else:
                result.differences.append(diff)
        return result

    def sequence(self, name: str, gate: str, left: list, right: list) -> GroupResult:
        result = GroupResult(name, gate, len(left), len(right))
        for index in range(max(len(left), len(right))):
            lv = left[index] if index < len(left) else None
            rv = right[index] if index < len(right) else None
            key = f"[{index}]"
            if index >= len(right):
                result.differences.append(Difference(name, "-", key, left=lv))
            elif index >= len(left):
                result.differences.append(Difference(name, "+", key, right=rv))
            elif _canon(lv, self.decimals) != _canon(rv, self.decimals):
                diff = Difference(name, "~", key, left=lv, right=rv)
                if self._float_noise(lv, rv):
                    diff.note = "float-noise"
                    result.notes.append(diff)
                else:
                    result.differences.append(diff)
        return result

    def multiset(self, name: str, gate: str, left: list, right: list) -> GroupResult:
        result = GroupResult(name, gate, len(left), len(right))
        lc: dict[str, int] = {}
        rc: dict[str, int] = {}
        for item in left:
            key = _key(item, self.decimals)
            lc[key] = lc.get(key, 0) + 1
        for item in right:
            key = _key(item, self.decimals)
            rc[key] = rc.get(key, 0) + 1
        for key in sorted(set(lc) | set(rc)):
            ln, rn = lc.get(key, 0), rc.get(key, 0)
            if ln == rn:
                continue
            if rn == 0:
                result.differences.append(Difference(name, "-", key, left=f"x{ln}"))
            elif ln == 0:
                result.differences.append(Difference(name, "+", key, right=f"x{rn}"))
            else:
                result.differences.append(
                    Difference(name, "~", key, left=f"x{ln}", right=f"x{rn}")
                )
        return result


# --- per-group reductions -------------------------------------------------


def _matrix_map(doc: dict) -> tuple[dict[str, float], int]:
    """Clearance matrix → `"(a,b)@layer" -> value`, projected onto the diagonal.

    See the "clearance-matrix off-diagonal" exclusion: the release writer emits
    only diagonal cells, so an off-diagonal cell on one side and not the other
    would be an artefact of the oracle, not a parity failure. They are dropped
    and counted.
    """
    out: dict[str, float] = {}
    dropped = 0
    for entry in doc.get("rules", {}).get("clearance_matrix", []) or []:
        classes = list(entry.get("classes") or [])
        if len(classes) != 2:
            continue
        a, b = sorted(classes)
        if a != b:
            dropped += 1
            continue
        out[f"({a},{b})@{entry.get('layer')}"] = entry.get("value")
    return out, dropped


def _bag(items: Iterable[str]) -> dict[str, int]:
    """Multiset as a name-keyed map, so a difference names the member."""
    out: dict[str, int] = {}
    for item in items:
        out[item] = out.get(item, 0) + 1
    return out


def _padstack_map(doc: dict) -> dict[str, Any]:
    out: dict[str, Any] = {}
    for entry in doc.get("padstacks", []) or []:
        # A padstack holds at most one shape per layer (`Padstack.get_shape`),
        # so keying by layer localises "the F.Cu pad grew" to one line.
        shapes: dict[str, Any] = {}
        for shape in entry.get("shapes", []) or []:
            layer = str(shape.get("layer"))
            record = [shape.get("kind"), shape.get("coords")]
            if layer in shapes:
                shapes[layer].append(record)
            else:
                shapes[layer] = [record]
        for layer in shapes:
            shapes[layer].sort(key=lambda s: _key(s, DEFAULT_DECIMALS))
        out[entry.get("name", "")] = {
            "attach": entry.get("attach"),
            "absolute": entry.get("absolute"),
            "shapes": shapes,
        }
    return out


def _java_round(value: float) -> int:
    """`java.lang.Math.round(double)` — round half *up*, towards positive infinity.

    Not Python's `round` (half to even) and not `math.floor(x + 0.5)` (wrong for
    `0.49999999999999994`, whose sum with a half rounds up to exactly 1.0).
    """
    floor = math.floor(value)
    return int(floor + 1 if value - floor >= 0.5 else floor)


def _component_map(doc: dict) -> dict[str, Any]:
    out: dict[str, Any] = {}
    for entry in doc.get("components", []) or []:
        # `Component.write_scope` writes the placement angle as
        # `(int) Math.round(get_rotation_in_degree())` (`Component.java:67`), so the dump
        # cannot carry a fractional rotation and the oracle is blind below one degree. Both
        # sides are put through the same rounding — see the `component rotation` exclusion.
        rotation = entry.get("rotation")
        if isinstance(rotation, (int, float)) and not isinstance(rotation, bool):
            rotation = _java_round(float(rotation))
        out[entry.get("name", "")] = {
            "image": entry.get("image"),
            "placed": entry.get("placed"),
            "x": entry.get("x"),
            "y": entry.get("y"),
            "side": entry.get("side"),
            "rotation": rotation,
            "position_fixed": entry.get("position_fixed"),
            "pin": {
                str(p.get("name")): p.get("clearance_class")
                for p in entry.get("pins", []) or []
            },
            "keepout": {
                f"{k.get('kind')}:{k.get('name')}": k.get("clearance_class")
                for k in entry.get("keepouts", []) or []
            },
        }
    return out


def _image_map(doc: dict) -> dict[str, Any]:
    out: dict[str, Any] = {}
    for entry in doc.get("images", []) or []:
        out[entry.get("name", "")] = {
            "side": entry.get("side"),
            "pin": {
                str(p.get("name")): [
                    p.get("padstack"),
                    p.get("x"),
                    p.get("y"),
                    p.get("rotation"),
                ]
                for p in entry.get("pins", []) or []
            },
            "keepouts": entry.get("keepouts"),
            "via_keepouts": entry.get("via_keepouts"),
            "place_keepouts": entry.get("place_keepouts"),
            "outline": _bag(
                _key(o, DEFAULT_DECIMALS) for o in entry.get("outlines", []) or []
            ),
        }
    return out


def _net_map(doc: dict) -> dict[str, Any]:
    out: dict[str, Any] = {}
    for entry in doc.get("nets", []) or []:
        key = f"{entry.get('name')}#{entry.get('subnet')}"
        pins = _bag(f"{p[0]}-{p[1]}" for p in entry.get("pins", []) or [])
        if key in out:  # a name/subnet pair should be unique; merge if not
            for pin, count in pins.items():
                out[key]["pin"][pin] = out[key]["pin"].get(pin, 0) + count
            out[key]["duplicate_scopes"] = out[key].get("duplicate_scopes", 1) + 1
        else:
            out[key] = {"pin": pins}
    return out


def _net_class_map(doc: dict) -> dict[str, Any]:
    out: dict[str, Any] = {}
    for entry in doc.get("net_classes", []) or []:
        out[entry.get("name", "")] = {
            "nets": sorted(entry.get("nets", []) or []),
            "clearance_class": entry.get("clearance_class"),
            "via_rule": entry.get("via_rule"),
            "trace_width": entry.get("trace_width"),
            "use_layer": sorted(entry.get("use_layer", []) or []),
            "length_max": entry.get("length_max"),
            "length_min": entry.get("length_min"),
            "pull_tight": entry.get("pull_tight"),
            "shove_fixed": entry.get("shove_fixed"),
        }
    return out


def _via_rule_items(doc: dict) -> list[Any]:
    """Via infos, via rules and the structure padstack list, as one multiset.

    All three are `via rules` in the G1.1 sense and all three are written in
    Java-internal iteration order, hence the multiset. Multiplicity is kept:
    upstream really does emit duplicate `(via_rule …)` records (Issue026 emits
    `default` twice) and dropping the duplicate would hide a divergence.
    """
    items: list[Any] = []
    for entry in doc.get("via_infos", []) or []:
        items.append(
            [
                "via_info",
                entry.get("name"),
                entry.get("padstack"),
                entry.get("clearance_class"),
                bool(entry.get("attach")),
            ]
        )
    for entry in doc.get("via_rules", []) or []:
        items.append(["via_rule", entry.get("name"), list(entry.get("vias") or [])])
    for name in doc.get("via_padstacks", []) or []:
        items.append(["via_padstack", name])
    return items


def _wiring_map(doc: dict) -> dict[str, int]:
    out: dict[str, int] = {}
    for entry in doc.get("wiring", {}).get("by_kind_fixed", []) or []:
        kind = entry.get("kind")
        # `Wiring.write_conduction_area_scope` writes no `(type ...)`, so the dump's conduction
        # areas are always `unfixed` while the model's plane-derived ones are `SYSTEM_FIXED`.
        # Fold the fixed state away for that kind only — see the exclusion of the same name.
        fixed = "unfixed" if kind == "conduction_area" else entry.get("fixed")
        key = f"{kind}/{fixed}"
        out[key] = out.get(key, 0) + (entry.get("count") or 0)
    return out


def _misc_map(doc: dict) -> dict[str, Any]:
    rules = doc.get("rules", {}) or {}
    return {
        "snap_angle": rules.get("snap_angle"),
        "via_at_smd": rules.get("via_at_smd"),
        "default_trace_width": rules.get("default_trace_width"),
        "smd_to_turn_gap": rules.get("smd_to_turn_gap"),
    }


# Field groups, in report order. `gate` marks the ones `docs/ROADMAP.md` G1.1
# names; `extra` groups are compared too (they are free, and they localise a
# failure faster) but `--gate-only` drops them.
GROUPS: list[str] = [
    "resolution",
    "layers",
    "clearance_matrix",
    "padstacks",
    "components",
    "nets",
    "wiring",
    "via_rules",
    "images",
    "net_classes",
    "rules_misc",
    "boundary",
    "structure_areas",
]

GATE_GROUPS = {
    "resolution",
    "layers",
    "clearance_matrix",
    "padstacks",
    "components",
    "nets",
    "wiring",
    "via_rules",
}


def compare(
    left: dict,
    right: dict,
    fixture: str = "",
    left_label: str = "java",
    right_label: str = "ours",
    decimals: int = DEFAULT_DECIMALS,
    gate_only: bool = False,
) -> CompareResult:
    """Compare two schema documents group by group."""
    result = CompareResult(fixture, left_label, right_label)
    for doc, side in ((left, left_label), (right, right_label)):
        got = doc.get("schema")
        if got != SCHEMA_ID:
            result.errors.append(f"{side}: schema is {got!r}, expected {SCHEMA_ID!r}")
    if result.errors:
        return result

    differ = _Differ(decimals)
    left_matrix, left_dropped = _matrix_map(left)
    right_matrix, right_dropped = _matrix_map(right)

    built: dict[str, GroupResult] = {
        "resolution": differ.mapping(
            "resolution",
            "G1.1",
            {"unit": left.get("unit"), **{f"resolution.{k}": v for k, v in (left.get("resolution") or {}).items()}},
            {"unit": right.get("unit"), **{f"resolution.{k}": v for k, v in (right.get("resolution") or {}).items()}},
        ),
        "layers": differ.sequence(
            "layers", "G1.1", left.get("layers") or [], right.get("layers") or []
        ),
        "clearance_matrix": differ.mapping(
            "clearance_matrix",
            "G1.1",
            left_matrix,
            right_matrix,
            excluded=left_dropped + right_dropped,
        ),
        "padstacks": differ.mapping(
            "padstacks", "G1.1", _padstack_map(left), _padstack_map(right)
        ),
        "components": differ.mapping(
            "components", "G1.1", _component_map(left), _component_map(right)
        ),
        "nets": differ.mapping("nets", "G1.1", _net_map(left), _net_map(right)),
        "wiring": differ.mapping("wiring", "G1.1", _wiring_map(left), _wiring_map(right)),
        "via_rules": differ.multiset(
            "via_rules", "G1.1", _via_rule_items(left), _via_rule_items(right)
        ),
        "images": differ.mapping("images", "extra", _image_map(left), _image_map(right)),
        "net_classes": differ.mapping(
            "net_classes", "extra", _net_class_map(left), _net_class_map(right)
        ),
        "rules_misc": differ.mapping(
            "rules_misc", "extra", _misc_map(left), _misc_map(right)
        ),
        "boundary": differ.multiset(
            "boundary", "extra", left.get("boundary") or [], right.get("boundary") or []
        ),
        "structure_areas": differ.multiset(
            "structure_areas",
            "extra",
            left.get("structure_areas") or [],
            right.get("structure_areas") or [],
        ),
    }
    for name in GROUPS:
        group = built[name]
        if gate_only and group.gate != "G1.1":
            continue
        result.groups.append(group)
    return result


# ==========================================================================
# Schema validation
# ==========================================================================

_REQUIRED_TOP = (
    "schema",
    "resolution",
    "unit",
    "layers",
    "rules",
    "padstacks",
    "images",
    "components",
    "nets",
    "net_classes",
    "via_infos",
    "via_rules",
    "via_padstacks",
    "wiring",
    "boundary",
    "structure_areas",
)


def validate_document(doc: dict) -> list[str]:
    """Structural check of a schema document; returns a list of problems."""
    problems: list[str] = []
    if doc.get("schema") != SCHEMA_ID:
        problems.append(f"schema is {doc.get('schema')!r}, expected {SCHEMA_ID!r}")
    for key in _REQUIRED_TOP:
        if key not in doc:
            problems.append(f"missing top-level key {key!r}")
    for key in (
        "layers",
        "padstacks",
        "images",
        "components",
        "nets",
        "net_classes",
        "via_infos",
        "via_rules",
        "via_padstacks",
        "boundary",
        "structure_areas",
    ):
        if key in doc and not isinstance(doc[key], list):
            problems.append(f"{key} must be a list")
    if isinstance(doc.get("layers"), list):
        for index, layer in enumerate(doc["layers"]):
            if not isinstance(layer, dict) or "name" not in layer or "type" not in layer:
                problems.append(f"layers[{index}] must be {{name, type}}")
            elif layer.get("type") not in ("signal", "power", None):
                problems.append(f"layers[{index}].type = {layer.get('type')!r}")
    rules = doc.get("rules")
    if not isinstance(rules, dict):
        problems.append("rules must be an object")
    elif not isinstance(rules.get("clearance_matrix"), list):
        problems.append("rules.clearance_matrix must be a list")
    else:
        for index, entry in enumerate(rules["clearance_matrix"]):
            classes = entry.get("classes") if isinstance(entry, dict) else None
            if not isinstance(classes, list) or len(classes) != 2:
                problems.append(f"rules.clearance_matrix[{index}].classes must be a 2-list")
            if not isinstance(entry, dict) or not isinstance(
                entry.get("value"), (int, float)
            ):
                problems.append(f"rules.clearance_matrix[{index}].value must be a number")
    wiring = doc.get("wiring")
    if not isinstance(wiring, dict) or not isinstance(wiring.get("by_kind_fixed"), list):
        problems.append("wiring.by_kind_fixed must be a list")
    else:
        for index, entry in enumerate(wiring["by_kind_fixed"]):
            if entry.get("kind") not in ("trace", "via", "conduction_area"):
                problems.append(f"wiring.by_kind_fixed[{index}].kind = {entry.get('kind')!r}")
            if entry.get("fixed") not in (
                "unfixed",
                "shove_fixed",
                "system_fixed",
                "user_fixed",
            ):
                problems.append(f"wiring.by_kind_fixed[{index}].fixed = {entry.get('fixed')!r}")
    for index, net in enumerate(doc.get("nets") or []):
        for pin in net.get("pins") or []:
            if not isinstance(pin, list) or len(pin) != 2:
                problems.append(f"nets[{index}].pins entries must be [component, pin]")
                break
    return problems


def document_stats(doc: dict) -> dict[str, int]:
    """Cheap size summary, used by the `--all` table."""
    matrix, _ = _matrix_map(doc)
    wiring = _wiring_map(doc)
    return {
        "layers": len(doc.get("layers") or []),
        "clearance": len(matrix),
        "padstacks": len(doc.get("padstacks") or []),
        "images": len(doc.get("images") or []),
        "components": len(doc.get("components") or []),
        "pins": sum(len(c.get("pins") or []) for c in doc.get("components") or []),
        "nets": len(doc.get("nets") or []),
        "net_pins": sum(len(n.get("pins") or []) for n in doc.get("nets") or []),
        "wiring": sum(wiring.values()),
        "via_rules": len(_via_rule_items(doc)),
    }


# ==========================================================================
# Loading
# ==========================================================================


def load_document(path: str) -> dict:
    """Load either a schema JSON document or a DSN dump (adapted on the fly)."""
    with open(path, "rb") as handle:
        head = handle.read(4096)
    text_head = head.decode("utf-8", errors="replace").lstrip()
    if text_head.startswith("{"):
        with open(path, "r", encoding="utf-8") as handle:
            doc = json.load(handle)
        if not isinstance(doc, dict):
            raise DumpParseError(f"{path}: JSON root is not an object")
        return doc
    return parse_java_dump(path)


def fixture_name(path: str) -> str:
    base = os.path.basename(path)
    for suffix in (".import.dsn", ".golden.json", ".json", ".dsn"):
        if base.endswith(suffix):
            return base[: -len(suffix)]
    return os.path.splitext(base)[0]


def expected_fixtures(manifest_path: str = DEFAULT_MANIFEST) -> list[str]:
    """Fixture stems the manifest declares, for the absent-dump report."""
    try:
        with open(manifest_path, "r", encoding="utf-8") as handle:
            manifest = json.load(handle)
    except (OSError, ValueError):
        return []
    names = []
    for entry in manifest.get("fixtures") or []:
        name = entry.get("name") or ""
        if name.endswith(".dsn"):
            name = name[: -len(".dsn")]
        if name:
            names.append(name)
    return names


# ==========================================================================
# Reporting
# ==========================================================================


def render_report(result: CompareResult, max_diffs: int = 10, verbose: bool = False) -> str:
    lines: list[str] = []
    lines.append(f"fixture : {result.fixture or '(unnamed)'}")
    lines.append(f"left    : {result.left_label}")
    lines.append(f"right   : {result.right_label}")
    lines.append("")
    if result.errors:
        for err in result.errors:
            lines.append(f"ERROR: {err}")
        return "\n".join(lines)

    lines.append(f"{'group':<18}{'gate':<7}{'status':<10}{'left':>8}{'right':>8}{'diffs':>7}")
    lines.append("-" * 58)
    for group in result.groups:
        status = "OK" if group.ok else "MISMATCH"
        lines.append(
            f"{group.name:<18}{group.gate:<7}{status:<10}"
            f"{group.left_size:>8}{group.right_size:>8}{len(group.differences):>7}"
        )
    lines.append("-" * 58)
    lines.append(
        f"{'TOTAL':<18}{'':<7}{'CLEAN' if result.ok else 'DIFFER':<10}"
        f"{'':>8}{'':>8}{result.diff_count:>7}"
    )

    for group in result.groups:
        if not group.differences:
            continue
        lines.append("")
        lines.append(f"{group.name} ({len(group.differences)} difference(s))")
        shown = group.differences if verbose else group.differences[:max_diffs]
        for diff in shown:
            lines.append(diff.render())
        if len(shown) < len(group.differences):
            lines.append(f"  … and {len(group.differences) - len(shown)} more")

    notes = [(g, d) for g in result.groups for d in g.notes]
    if notes:
        lines.append("")
        lines.append(f"float-noise (within one quantum, not counted): {len(notes)}")
        for group, diff in notes[: max_diffs if not verbose else len(notes)]:
            lines.append(f"  ~ {group.name}/{diff.key}: {_short(diff.left)} vs {_short(diff.right)}")

    excluded = sum(g.excluded for g in result.groups)
    if excluded:
        lines.append("")
        lines.append(
            f"excluded off-diagonal clearance cells: {excluded} "
            "(not observable through the release dump — see EXCLUSIONS)"
        )
    return "\n".join(lines)


def result_to_json(result: CompareResult) -> dict:
    return {
        "fixture": result.fixture,
        "left": result.left_label,
        "right": result.right_label,
        "ok": result.ok,
        "errors": result.errors,
        "differences": result.diff_count,
        "groups": [
            {
                "name": g.name,
                "gate": g.gate,
                "ok": g.ok,
                "left_size": g.left_size,
                "right_size": g.right_size,
                "excluded": g.excluded,
                "notes": len(g.notes),
                "differences": [
                    {
                        "op": d.op,
                        "key": d.key,
                        "left": d.left,
                        "right": d.right,
                        "note": d.note,
                    }
                    for d in g.differences
                ],
            }
            for g in result.groups
        ],
    }


_TABLE_COLUMNS = [
    ("layers", 7),
    ("clearance", 10),
    ("padstacks", 10),
    ("components", 11),
    ("nets", 6),
    ("net_pins", 9),
    ("wiring", 8),
    ("via_rules", 10),
]


# A dump whose own Java run failed part-way is not a resolved board model, and
# comparing against it measures the failure, not our importer. The two markers
# below are the release jar's *own* words for "I did not finish reading this
# board"; both are read out of the sibling `<fixture>.log` written by the same
# run, so this is evidence per run, never a hard-coded fixture list.
#
#   `parse error while reading board file` — `DsnFile.read` rejected the header
#       outright (`Issue006-LPC18XX_43XX_SCH` is an OLE2 compound document, and
#       the manifest files it under `coverage/not_specctra_dsn`). The dump next
#       to that log does not describe the fixture at all.
#   `Component.read_place_scope: ) expected` / `read_string_scope: closing
#       bracket expected` — the placement scope died mid-scan
#       (`Issue229-display-8-digit-hc595` has a `'` inside a `(PN …)`, which the
#       scanner takes as a string quote). Every later scope — placement,
#       library, network, wiring — comes out empty.
#
# Each marker must name a site that *aborts*. `Component.read_place_scope` has
# four `FRLogger.warn` sites (`parser/Component.java:185,195,204,246`) and three
# of them `return null`, but `:195` ("Keyword.FRONT expected") only defaults the
# side to front and reads on — a board that trips it still yields a complete,
# comparable dump. Matching the bare method name would silently drop such a board
# out of the gate's denominator, so the aborting message is spelled out in full.
ORACLE_READ_FAILURE_MARKERS = (
    "parse error while reading board file",
    "Component.read_place_scope: ) expected",
    "Component.read_place_scope: number expected",
    "Component.read_place_scope: Double was expected",
    "read_string_scope: closing bracket expected",
)


def oracle_read_failure(dump_path: str) -> str | None:
    """The marker proving the release jar did not finish reading this fixture, if any."""
    log_path = dump_path[: -len(".import.dsn")] + ".log" if dump_path.endswith(".import.dsn") else ""
    if not log_path or not os.path.exists(log_path):
        return None
    try:
        with open(log_path, encoding="utf-8", errors="replace") as handle:
            text = handle.read()
    except OSError:
        return None
    return next((m for m in ORACLE_READ_FAILURE_MARKERS if m in text), None)


def render_sweep(
    rows: list[dict],
    absent: list[str],
    gate_only: bool,
    failed_oracles: Sequence[tuple[str, str]] = (),
) -> str:
    lines: list[str] = []
    header = f"{'fixture':<46}" + "".join(f"{name:>{width}}" for name, width in _TABLE_COLUMNS)
    header += f"{'diffs':>7}  result"
    lines.append(header)
    lines.append("-" * len(header))
    for row in rows:
        stats = row.get("stats") or {}
        line = f"{row['fixture']:<46}"
        line += "".join(f"{stats.get(name, '-'):>{width}}" for name, width in _TABLE_COLUMNS)
        line += f"{row.get('differences', 0):>7}  {row['result']}"
        lines.append(line)
    lines.append("-" * len(header))
    comparable = [row for row in rows if row["result"] != "ORACLE-FAILED"]
    clean = sum(1 for row in comparable if row["result"] == "OK")
    lines.append(
        f"{clean}/{len(comparable)} clean"
        + ("  (G1.1 groups only)" if gate_only else "  (G1.1 + extra groups)")
    )
    if failed_oracles:
        lines.append("")
        lines.append(
            f"unusable oracles ({len(failed_oracles)}) — the release jar's own read of the "
            "fixture failed, so its dump is not a resolved board model and is not compared:"
        )
        for name, marker in failed_oracles:
            lines.append(f"  - {name}: log says {marker!r}")
    if absent:
        lines.append("")
        lines.append(
            f"absent dumps ({len(absent)}) — Java hangs or crashes importing these, "
            "so no oracle exists and G1.1 cannot cover them:"
        )
        for name in absent:
            lines.append(f"  - {name}")
    return "\n".join(lines)


# ==========================================================================
# CLI
# ==========================================================================


def _sweep(args: argparse.Namespace) -> int:
    dump_dir = args.dump_dir
    paths = sorted(glob.glob(os.path.join(dump_dir, "*.import.dsn")))
    if not paths:
        print(f"no dumps found in {dump_dir}", file=sys.stderr)
        return 2
    present = {fixture_name(p) for p in paths}
    absent = [name for name in expected_fixtures(args.manifest) if name not in present]

    rows: list[dict] = []
    exit_code = 0
    json_results: list[dict] = []
    failed_oracles: list[tuple[str, str]] = []
    for path in paths:
        fixture = fixture_name(path)
        row: dict[str, Any] = {"fixture": fixture}
        # Only meaningful when there is a `--ours-dir`: the self-comparison is a
        # harness test of the parser and stays green even on a truncated dump.
        marker = oracle_read_failure(path) if args.ours_dir else None
        if marker is not None:
            failed_oracles.append((fixture, marker))
            row["result"] = "ORACLE-FAILED"
            row["differences"] = 0
            row["oracle_failure"] = marker
            rows.append(row)
            continue
        try:
            left = parse_java_dump(path)
        except Exception as exc:  # noqa: BLE001 — one bad dump must not stop the sweep
            row["result"] = f"PARSE-ERROR: {exc}"
            row["differences"] = 0
            rows.append(row)
            exit_code = 1
            continue
        problems = validate_document(left)
        if problems:
            row["result"] = f"SCHEMA-ERROR: {problems[0]}"
            row["differences"] = len(problems)
            rows.append(row)
            exit_code = 1
            continue
        row["stats"] = document_stats(left)

        if args.ours_dir:
            candidates = [
                os.path.join(args.ours_dir, fixture + ext)
                for ext in (".golden.json", ".json", ".import.dsn", ".dsn")
            ]
            ours_path = next((c for c in candidates if os.path.exists(c)), None)
            if ours_path is None:
                row["result"] = "MISSING-OURS"
                row["differences"] = 0
                rows.append(row)
                exit_code = 1
                continue
            right = load_document(ours_path)
            right_label = ours_path
        else:
            # Self-comparison: round-trip the adapted document through JSON so
            # the sweep also proves the schema survives serialisation.
            right = json.loads(json.dumps(left, ensure_ascii=False))
            right_label = f"{path} (self, via JSON)"

        result = compare(
            left,
            right,
            fixture=fixture,
            left_label=path,
            right_label=right_label,
            decimals=args.float_decimals,
            gate_only=args.gate_only,
        )
        row["result"] = "OK" if result.ok else "MISMATCH"
        row["differences"] = result.diff_count
        rows.append(row)
        json_results.append(result_to_json(result))
        if not result.ok:
            exit_code = 1
            if args.verbose:
                print(render_report(result, args.max_diffs, args.verbose))
                print()

    if args.json:
        print(
            json.dumps(
                {
                    "dump_dir": dump_dir,
                    "absent": absent,
                    "failed_oracles": [
                        {"fixture": name, "marker": marker} for name, marker in failed_oracles
                    ],
                    "rows": rows,
                    "results": json_results,
                },
                indent=2,
                ensure_ascii=False,
            )
        )
    else:
        print(render_sweep(rows, absent, args.gate_only, failed_oracles))
    return exit_code


def _pair(args: argparse.Namespace) -> int:
    try:
        left = load_document(args.java_dump)
        right = load_document(args.ours)
    except (OSError, ValueError, DumpParseError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    problems = validate_document(left) + validate_document(right)
    if problems:
        for problem in problems:
            print(f"schema error: {problem}", file=sys.stderr)
        return 1
    result = compare(
        left,
        right,
        fixture=fixture_name(args.java_dump),
        left_label=args.java_dump,
        right_label=args.ours,
        decimals=args.float_decimals,
        gate_only=args.gate_only,
    )
    if args.json:
        print(json.dumps(result_to_json(result), indent=2, ensure_ascii=False))
    else:
        print(render_report(result, args.max_diffs, args.verbose))
    return 0 if result.ok else 1


def _emit(args: argparse.Namespace) -> int:
    """Adapt a Java dump to the schema and print it — the reference encoder."""
    try:
        doc = parse_java_dump(args.emit)
    except (OSError, DumpParseError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    print(json.dumps(doc, indent=2, ensure_ascii=False))
    return 0


def _exclusions() -> int:
    import textwrap

    print("Known-lossy exclusions — normalised away before comparison.")
    print("Normative discussion: bench/tools/README.md.\n")
    for index, (title, body) in enumerate(EXCLUSIONS, start=1):
        print(f"{index}. {title}")
        print(textwrap.fill(body, width=78, initial_indent="   ", subsequent_indent="   "))
        print()
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="golden_compare.py",
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--java-dump", help="Java `-do` dump (*.import.dsn)")
    parser.add_argument("--ours", help="our dump: schema JSON, or a DSN dump to adapt")
    parser.add_argument("--all", action="store_true", help="sweep every fixture")
    parser.add_argument("--ours-dir", help="directory of our dumps, for --all")
    parser.add_argument("--dump-dir", default=DEFAULT_DUMP_DIR, help="Java dump directory")
    parser.add_argument("--manifest", default=DEFAULT_MANIFEST, help="fixture manifest")
    parser.add_argument("--emit", help="adapt one Java dump to schema JSON and print it")
    parser.add_argument("--gate-only", action="store_true", help="compare only G1.1 groups")
    parser.add_argument(
        "--float-decimals",
        type=int,
        default=DEFAULT_DECIMALS,
        help=f"decimals every number is quantised to (default {DEFAULT_DECIMALS})",
    )
    parser.add_argument("--max-diffs", type=int, default=10, help="differences shown per group")
    parser.add_argument("-v", "--verbose", action="store_true", help="show every difference")
    parser.add_argument("--json", action="store_true", help="machine-readable output")
    parser.add_argument("--exclusions", action="store_true", help="print the lossy exclusions")
    parser.add_argument("--self-test", action="store_true", help="run the unit tests")
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    if args.self_test:
        return _run_self_test()
    if args.exclusions:
        return _exclusions()
    if args.emit:
        return _emit(args)
    if args.all:
        return _sweep(args)
    if args.java_dump and args.ours:
        return _pair(args)
    build_parser().print_help()
    return 2


def _run_self_test() -> int:
    import unittest

    tests_dir = os.path.join(os.path.dirname(os.path.abspath(__file__)), "tests")
    loader = unittest.TestLoader()
    suite = loader.discover(tests_dir, pattern="test_golden_compare.py", top_level_dir=tests_dir)
    runner = unittest.TextTestRunner(verbosity=2)
    return 0 if runner.run(suite).wasSuccessful() else 1


if __name__ == "__main__":
    sys.exit(main())
