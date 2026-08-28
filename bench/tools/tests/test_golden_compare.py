#!/usr/bin/env python3
"""Tests for `bench/tools/golden_compare.py`.

Three layers, in increasing cost:

1. **Unit** — the dump canonicaliser, the clearance-`(type …)` normaliser, the
   rule/shape readers, the comparator's three difference shapes.
2. **Property** — the comparator must be blind to exactly the three documented
   lossy axes (ordering, float formatting, class-pair spelling) and sighted for
   everything else. Both halves are asserted; a comparator that is blind to
   everything passes only the first.
3. **Fixture sweep** — every dump in `bench/golden/dumps/` is parsed, schema
   validated, round-tripped through JSON, self-compared (must be clean), and
   checked for vacuity. This is the G1.0 harness gate: it proves the parser and
   the comparator on all 43 boards without any Rust in the loop.

Run: `python3 bench/tools/golden_compare.py --self-test`
 or: `python3 -m unittest discover -s bench/tools/tests`
"""

from __future__ import annotations

import copy
import glob
import json
import os
import random
import sys
import tempfile
import unittest

_TOOLS_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
_BENCH_DIR = os.path.dirname(_TOOLS_DIR)
for _p in (_TOOLS_DIR, _BENCH_DIR):
    if _p not in sys.path:
        sys.path.insert(0, _p)

import check_ses  # noqa: E402
import golden_compare as gc  # noqa: E402

DUMP_DIR = os.path.join(_BENCH_DIR, "golden", "dumps")


def dump_paths() -> list[str]:
    return sorted(glob.glob(os.path.join(DUMP_DIR, "*.import.dsn")))


# A complete, minimal board that exercises every scope the adapter reads. Kept
# inline so the unit tests run with no fixtures on disk.
MINI_DUMP = """(pcb "mini.import"
  (parser
    (string_quote ")
    (space_in_quoted_tokens on)
    (host_cad "KiCad's Pcbnew")
    (generated_by_freerouting)
  )
  (resolution um 10)
  (unit um)
  (structure
    (layer F.Cu
      (type signal)
    )
    (layer "In1.Cu"
      (type power)
      (rule
        (width 300.0)
        (clearance 210.0)
        (clearance 130.0 (type smd_to_turn_gap))
        (clearance 60.0 (type smd))
      )
    )
    (boundary
      (rect pcb 0.0 -100.0 200.0 0.0)
    )
    (via "V1" "V1")
    (rule
      (width 250.0)
      (clearance 200.0)
      (clearance 125.0 (type smd_to_turn_gap))
      (clearance 50.0 (type smd))
      (clearance 500.0 (type "board_edge"))
    )
    (snap_angle
      fortyfive_degree
    )
    (control
      (via_at_smd off)
    )
    (keepout
      (polygon F.Cu 0
        1.0 -1.0
        2.0 -1.0
        2.0 -2.0
      )
      (clearance_class "kp")
    )
    (plane GND
      (polygon "In1.Cu" 0
        0.0 0.0
        10.0 0.0
        10.0 -10.0
      )
    )
  )
  (placement
    (component "PKG-A"
      (place
        "U-1" 100.0 -50.0 back 270
        (lock_type position)
        (pin "1" (clearance_class smd))
        (pin P2 (clearance_class default))
        (keepout "ko_1" (clearance_class default))
      )
    )
  )
  (library
    (image "PKG-A"
      (side front)
      (pin "PAD_R" "1" -10.0 5.0)
      (pin "PAD_R" P2 10.0 5.0(rotate 90))
      (outline
        (polygon signal 0
          -1.0 -1.0
          1.0 -1.0
          1.0 1.0
        )
      )
    )
    (padstack "PAD_R"
      (shape
        (rect F.Cu -5.0 -10.0 5.0 10.0)
      )
      (attach off)
    )
    (padstack V1
      (shape
        (circle F.Cu 800.0 0.0 0.0)
      )
      (shape
        (circle "In1.Cu" 800.0 0.0 0.0)
      )
    )
  )
  (network
    (net GND 1
      (pins
        "U-1"-"1"
        "U-1"-P2
      )
    )
    (via
      V1 V1 default
    )
    (via_rule
      default V1
    )
    (class default
      GND
      (clearance_class default)
      (via_rule default)
      (rule
        (width 250.0)
      )
      (circuit
        (use_layer F.Cu "In1.Cu")
      )
    )
  )
  (wiring
    (wire
      (polyline_path F.Cu 250.0
        0.0 0.0 1.0 0.0
      )
      (net GND 1)
      (clearance_class default)
      (type protect)
    )
    (wire
      (path F.Cu 250.0
        0.0 0.0
        1.0 1.0
      )
      (net GND 1)
      (clearance_class default)
    )
    (wire
      (polygon F.Cu 0
        0.0 0.0
        1.0 0.0
        1.0 1.0
      )
      (net GND 1)
      (clearance_class default)
    )
    (via
      V1 5.0 -5.0 (net GND 1)
      (clearance_class default)
      (type fix)
    )
    (via
      V1 6.0 -5.0 (net GND 1)
      (clearance_class default)
      (type shove_fixed)
    )
  )
)
"""


def mini_doc() -> dict:
    return gc.adapt_java_dump(MINI_DUMP, source="mini.import.dsn")


# ==========================================================================
# 1. Unit: canonicalisation
# ==========================================================================


class CanonicaliseTests(unittest.TestCase):
    def _atoms(self, text: str) -> list[str]:
        tree = check_ses.parse_sexp(gc.canonicalise_dump_text(text))
        return gc.raw_atoms(tree[0])

    def test_escape_round_trip(self):
        for name in ("plain", "with space", "a-b", "a_b", "(paren)", "%25", "Паяльная"):
            self.assertEqual(gc._unescape(gc._escape(name)), name)

    def test_escaped_atom_survives_the_tokenizer(self):
        self.assertEqual(
            [gc.decode(a) for a in self._atoms('(x "a b" "c-d" plain)')],
            ["a b", "c-d", "plain"],
        )

    def test_quoted_component_pin_ref_splits(self):
        # `Net.write_pin` emits `"COMP"-"PIN"`; the tolerant tokenizer alone
        # mis-scans it because the closing quote is followed by `-`.
        atoms = self._atoms('(pins\n "BLIT_BUS_SWITCH0"-"20"\n U2-"24"\n IC4-14\n)')
        self.assertEqual(
            [gc.split_ref(a) for a in atoms],
            [("BLIT_BUS_SWITCH0", "20"), ("U2", "24"), ("IC4", "14")],
        )

    def test_tolerant_tokenizer_alone_mangles_the_same_input(self):
        # Guards the reason canonicalisation exists: if check_ses ever starts
        # handling this, drop the pre-pass rather than keep two lexers.
        raw = check_ses.parse_sexp('(pins\n "BLIT_BUS_SWITCH0"-"20"\n)')
        self.assertEqual(raw[0][1], 'BLIT_BUS_SWITCH0"-"20')
        # It still splits, but both halves carry a stray quote — a silently
        # wrong component/pin name, which is worse than a loud failure.
        self.assertNotEqual(gc.split_ref(raw[0][1]), ("BLIT_BUS_SWITCH0", "20"))

    def test_hyphen_inside_a_quoted_half_is_not_a_separator(self):
        atoms = self._atoms('(pins "A-B"-"C_D")')
        self.assertEqual(gc.split_ref(atoms[0]), ("A-B", "C_D"))

    def test_string_quote_directive_is_stripped(self):
        text = gc.canonicalise_dump_text('(pcb x (parser (string_quote ")))')
        self.assertNotIn('"', text)

    def test_rejects_foreign_string_quote(self):
        with self.assertRaises(gc.DumpParseError):
            gc.canonicalise_dump_text("(pcb x (parser (string_quote ')))")

    def test_rejects_marker_characters_in_input(self):
        with self.assertRaises(gc.DumpParseError):
            gc.canonicalise_dump_text("(pcb \x02x)")

    def test_unterminated_quote_does_not_raise(self):
        atoms = self._atoms('(x "unterminated')
        self.assertEqual([gc.decode(a) for a in atoms], ["unterminated"])


# ==========================================================================
# 1. Unit: readers
# ==========================================================================


class ClearanceTypeTests(unittest.TestCase):
    def _type(self, text: str):
        tree = check_ses.parse_sexp(gc.canonicalise_dump_text(f"(clearance 1.0 {text})"))
        return gc.read_clearance_type(gc.first_child(tree[0], "type"))

    def test_smd_to_turn_gap_is_not_a_matrix_entry(self):
        self.assertIsNone(self._type("(type smd_to_turn_gap)"))

    def test_bare_single_name_is_the_diagonal(self):
        self.assertEqual(self._type("(type smd)"), ("smd", "smd"))

    def test_underscore_pair_normalises_to_the_same_diagonal(self):
        # The exclusion the plan review names: smd_smd -> smd.
        self.assertEqual(self._type("(type smd_smd)"), ("smd", "smd"))

    def test_hyphen_pair_form(self):
        self.assertEqual(self._type("(type smd-via)"), ("smd", "via"))

    def test_pair_is_unordered(self):
        self.assertEqual(self._type("(type via-smd)"), self._type("(type smd-via)"))

    def test_quoted_name_with_underscore_stays_one_class(self):
        # `IdentifierType` quotes any name containing `_`, so the quoting is
        # what tells `board_edge` (one class) from `smd_smd` (a pair).
        self.assertEqual(self._type('(type "board_edge")'), ("board_edge", "board_edge"))

    def test_quoted_name_with_hyphen_stays_one_class(self):
        self.assertEqual(self._type('(type "SIGNAL-via")'), ("SIGNAL-via", "SIGNAL-via"))


class RuleScopeTests(unittest.TestCase):
    def test_reads_width_default_gap_and_pairs(self):
        tree = check_ses.parse_sexp(
            gc.canonicalise_dump_text(
                "(rule (width 250.0) (clearance 200.0)"
                " (clearance 125.0 (type smd_to_turn_gap))"
                " (clearance 50.0 (type smd)))"
            )
        )
        rule = gc.read_rule_scope(tree[0])
        self.assertEqual(rule.width, 250.0)
        self.assertEqual(rule.default_clearance, 200.0)
        self.assertEqual(rule.smd_to_turn_gap, 125.0)
        self.assertEqual(rule.pairs, {("smd", "smd"): 50.0})
        self.assertTrue(rule.has_clearances)

    def test_width_only_rule_has_no_clearances(self):
        tree = check_ses.parse_sexp(gc.canonicalise_dump_text("(rule (width 1.0))"))
        self.assertFalse(gc.read_rule_scope(tree[0]).has_clearances)


class ShapeTests(unittest.TestCase):
    def _shape(self, text: str) -> dict:
        return gc.read_shape(check_ses.parse_sexp(gc.canonicalise_dump_text(text))[0])

    def test_circle(self):
        shape = self._shape("(circle F.Cu 800.0 1.0 2.0)")
        self.assertEqual(shape, {"kind": "circle", "layer": "F.Cu", "coords": [800.0, 1.0, 2.0]})
        self.assertEqual(gc._bbox("circle", [800.0, 0.0, 0.0]), [-400.0, -400.0, 400.0, 400.0])

    def test_rect_bbox_is_normalised(self):
        shape = self._shape("(rect F.Cu 5.0 6.0 -5.0 -6.0)")
        self.assertEqual(gc._bbox("rect", shape["coords"]), [-5.0, -6.0, 5.0, 6.0])

    def test_polygon_drops_the_aperture_from_the_bbox(self):
        shape = self._shape("(polygon B.Cu 0 1.0 2.0 3.0 4.0)")
        self.assertEqual(gc._bbox("polygon", shape["coords"]), [1.0, 2.0, 3.0, 4.0])
        self.assertEqual(gc._corner_count("polygon", shape["coords"]), 2)

    def test_polyline_path_corner_count_counts_lines(self):
        coords = [250.0] + [0.0] * 8
        self.assertEqual(gc._corner_count("polyline_path", coords), 2)


# ==========================================================================
# 1. Unit: the adapter on a synthetic board
# ==========================================================================


class AdapterTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.doc = mini_doc()

    def test_document_validates(self):
        self.assertEqual(gc.validate_document(self.doc), [])

    def test_resolution_and_unit(self):
        self.assertEqual(self.doc["resolution"], {"unit": "um", "value": 10.0})
        self.assertEqual(self.doc["unit"], "um")

    def test_layers_keep_order_and_type(self):
        self.assertEqual(
            self.doc["layers"],
            [{"name": "F.Cu", "type": "signal"}, {"name": "In1.Cu", "type": "power"}],
        )

    def test_clearance_matrix_expands_per_layer_with_the_layer_override(self):
        matrix, dropped = gc._matrix_map(self.doc)
        self.assertEqual(dropped, 0)
        self.assertEqual(
            matrix,
            {
                "(default,default)@F.Cu": 200.0,
                "(smd,smd)@F.Cu": 50.0,
                "(board_edge,board_edge)@F.Cu": 500.0,
                # In1.Cu carries its own (rule …), so it does NOT inherit
                # board_edge and it overrides default/smd.
                "(default,default)@In1.Cu": 210.0,
                "(smd,smd)@In1.Cu": 60.0,
            },
        )
        self.assertEqual(
            self.doc["rules"]["smd_to_turn_gap"], {"F.Cu": 125.0, "In1.Cu": 130.0}
        )
        self.assertEqual(
            self.doc["rules"]["default_trace_width"], {"F.Cu": 250.0, "In1.Cu": 300.0}
        )

    def test_snap_angle_and_via_at_smd(self):
        self.assertEqual(self.doc["rules"]["snap_angle"], "fortyfive_degree")
        self.assertEqual(self.doc["rules"]["via_at_smd"], "off")

    def test_padstacks(self):
        by_name = {p["name"]: p for p in self.doc["padstacks"]}
        self.assertEqual(
            by_name["PAD_R"]["shapes"],
            [{"kind": "rect", "layer": "F.Cu", "coords": [-5.0, -10.0, 5.0, 10.0]}],
        )
        # `(attach off)` present -> attach False; absent -> the model default True.
        self.assertFalse(by_name["PAD_R"]["attach"])
        self.assertTrue(by_name["V1"]["attach"])
        self.assertEqual([s["layer"] for s in by_name["V1"]["shapes"]], ["F.Cu", "In1.Cu"])

    def test_image_pins_carry_padstack_offset_and_rotation(self):
        image = self.doc["images"][0]
        self.assertEqual(image["side"], "front")
        self.assertEqual(
            image["pins"],
            [
                {"padstack": "PAD_R", "name": "1", "x": -10.0, "y": 5.0, "rotation": 0},
                {"padstack": "PAD_R", "name": "P2", "x": 10.0, "y": 5.0, "rotation": 90},
            ],
        )
        self.assertEqual(len(image["outlines"]), 1)

    def test_component_placement_and_pin_clearance_classes(self):
        comp = self.doc["components"][0]
        self.assertEqual(comp["name"], "U-1")
        self.assertEqual(comp["image"], "PKG-A")
        self.assertEqual((comp["x"], comp["y"]), (100.0, -50.0))
        self.assertEqual(comp["side"], "back")
        self.assertEqual(comp["rotation"], 270.0)
        self.assertTrue(comp["placed"])
        self.assertTrue(comp["position_fixed"])
        self.assertEqual(
            comp["pins"],
            [
                {"name": "1", "clearance_class": "smd"},
                {"name": "P2", "clearance_class": "default"},
            ],
        )
        self.assertEqual(
            comp["keepouts"],
            [{"kind": "keepout", "name": "ko_1", "clearance_class": "default"}],
        )

    def test_net_pin_membership(self):
        self.assertEqual(
            self.doc["nets"], [{"name": "GND", "subnet": 1, "pins": [["U-1", "1"], ["U-1", "P2"]]}]
        )

    def test_via_rules_and_infos(self):
        self.assertEqual(
            self.doc["via_infos"],
            [{"name": "V1", "padstack": "V1", "clearance_class": "default", "attach": False}],
        )
        self.assertEqual(self.doc["via_rules"], [{"name": "default", "vias": ["V1"]}])
        self.assertEqual(self.doc["via_padstacks"], ["V1", "V1"])

    def test_net_class(self):
        cls = self.doc["net_classes"][0]
        self.assertEqual(cls["name"], "default")
        self.assertEqual(cls["nets"], ["GND"])
        self.assertEqual(cls["clearance_class"], "default")
        self.assertEqual(cls["via_rule"], "default")
        self.assertEqual(cls["trace_width"], 250.0)
        self.assertEqual(cls["use_layer"], ["F.Cu", "In1.Cu"])
        self.assertTrue(cls["pull_tight"])
        self.assertFalse(cls["shove_fixed"])

    def test_wiring_counts_by_kind_and_fixed_state(self):
        self.assertEqual(
            _wiring_dict(self.doc),
            {
                # `path` and `polyline_path` are both traces; `polygon` is a
                # signal-layer conduction area.
                "trace/user_fixed": 1,  # (type protect) -> USER_FIXED
                "trace/unfixed": 1,
                "conduction_area/unfixed": 1,
                "via/system_fixed": 1,  # (type fix) -> SYSTEM_FIXED
                "via/shove_fixed": 1,
            },
        )

    def test_structure_areas_and_boundary(self):
        kinds = sorted(a["kind"] for a in self.doc["structure_areas"])
        self.assertEqual(kinds, ["keepout", "plane"])
        keepout = next(a for a in self.doc["structure_areas"] if a["kind"] == "keepout")
        self.assertEqual(keepout["layer"], "F.Cu")
        self.assertEqual(keepout["clearance_class"], "kp")
        plane = next(a for a in self.doc["structure_areas"] if a["kind"] == "plane")
        self.assertEqual(plane["net"], "GND")
        self.assertEqual(plane["layer"], "In1.Cu")
        self.assertEqual(len(self.doc["boundary"]), 1)

    def test_provenance_is_metadata_not_model(self):
        # `(pcb <name>)` and the parser scope are excluded from comparison.
        self.assertEqual(self.doc["design"], "mini.import")
        self.assertNotIn("host_cad", self.doc)


def _wiring_dict(doc: dict) -> dict[str, int]:
    return {f"{e['kind']}/{e['fixed']}": e["count"] for e in doc["wiring"]["by_kind_fixed"]}


# ==========================================================================
# 2. Comparator: sighted where it must be
# ==========================================================================


class ComparatorSightedTests(unittest.TestCase):
    def setUp(self):
        self.left = mini_doc()

    def _compare(self, right: dict) -> gc.CompareResult:
        return gc.compare(self.left, right, fixture="mini")

    def test_identical_documents_are_clean(self):
        result = self._compare(copy.deepcopy(self.left))
        self.assertTrue(result.ok, result.diff_count)
        self.assertEqual(result.diff_count, 0)

    def test_changed_clearance_value_is_caught(self):
        right = copy.deepcopy(self.left)
        right["rules"]["clearance_matrix"][1]["value"] = 51.0
        result = self._compare(right)
        self.assertFalse(result.ok)
        group = result.group("clearance_matrix")
        self.assertEqual([d.key for d in group.differences], ["(smd,smd)@F.Cu"])
        self.assertEqual(group.differences[0].op, "~")

    def test_missing_clearance_class_is_caught(self):
        right = copy.deepcopy(self.left)
        right["rules"]["clearance_matrix"] = [
            e for e in right["rules"]["clearance_matrix"] if e["classes"][0] != "board_edge"
        ]
        group = self._compare(right).group("clearance_matrix")
        self.assertEqual([(d.op, d.key) for d in group.differences], [("-", "(board_edge,board_edge)@F.Cu")])

    def test_layer_order_swap_is_caught(self):
        right = copy.deepcopy(self.left)
        right["layers"].reverse()
        result = self._compare(right)
        self.assertFalse(result.ok)
        self.assertEqual(len(result.group("layers").differences), 2)

    def test_layer_type_change_is_caught(self):
        right = copy.deepcopy(self.left)
        right["layers"][1]["type"] = "signal"
        self.assertFalse(self._compare(right).ok)

    def test_missing_component_is_caught(self):
        right = copy.deepcopy(self.left)
        right["components"] = []
        group = self._compare(right).group("components")
        self.assertEqual([(d.op, d.key) for d in group.differences], [("-", "U-1")])

    def test_component_side_flip_is_caught(self):
        right = copy.deepcopy(self.left)
        right["components"][0]["side"] = "front"
        self.assertFalse(self._compare(right).ok)

    def test_component_rotation_change_is_caught(self):
        right = copy.deepcopy(self.left)
        right["components"][0]["rotation"] = 90.0
        self.assertFalse(self._compare(right).ok)

    def test_pin_clearance_class_change_is_caught(self):
        right = copy.deepcopy(self.left)
        right["components"][0]["pins"][0]["clearance_class"] = "default"
        group = self._compare(right).group("components")
        self.assertEqual(len(group.differences), 1)

    def test_padstack_shape_layer_drop_is_caught(self):
        right = copy.deepcopy(self.left)
        by_name = {p["name"]: p for p in right["padstacks"]}
        by_name["V1"]["shapes"] = by_name["V1"]["shapes"][:1]
        self.assertFalse(self._compare(right).ok)

    def test_padstack_geometry_change_is_caught(self):
        right = copy.deepcopy(self.left)
        right["padstacks"][0]["shapes"][0]["coords"][0] = -6.0
        self.assertFalse(self._compare(right).ok)

    def test_extra_net_pin_is_caught(self):
        right = copy.deepcopy(self.left)
        right["nets"][0]["pins"].append(["U-1", "P3"])
        group = self._compare(right).group("nets")
        # The report names the pin, not just the net.
        self.assertEqual([(d.op, d.key) for d in group.differences], [("+", "GND#1.pin.U-1-P3")])

    def test_moved_net_pin_is_caught_on_both_nets(self):
        right = copy.deepcopy(self.left)
        right["nets"][0]["pins"] = [["U-1", "1"]]
        right["nets"].append({"name": "VCC", "subnet": 1, "pins": [["U-1", "P2"]]})
        group = self._compare(right).group("nets")
        self.assertEqual(
            sorted((d.op, d.key) for d in group.differences),
            [("+", "VCC#1"), ("-", "GND#1.pin.U-1-P2")],
        )

    def test_subnet_number_change_is_caught(self):
        right = copy.deepcopy(self.left)
        right["nets"][0]["subnet"] = 2
        group = self._compare(right).group("nets")
        self.assertEqual(sorted(d.op for d in group.differences), ["+", "-"])

    def test_wiring_count_change_is_caught(self):
        right = copy.deepcopy(self.left)
        right["wiring"]["by_kind_fixed"][0]["count"] += 1
        self.assertFalse(self._compare(right).ok)

    def test_fixed_state_reclassification_is_caught(self):
        # A trace that becomes SHOVE_FIXED instead of USER_FIXED keeps the total
        # but must still fail: G1.1 names counts by kind *and* fixed state.
        right = copy.deepcopy(self.left)
        for entry in right["wiring"]["by_kind_fixed"]:
            if entry["kind"] == "trace" and entry["fixed"] == "user_fixed":
                entry["fixed"] = "shove_fixed"
        group = self._compare(right).group("wiring")
        self.assertEqual(sorted(d.op for d in group.differences), ["+", "-"])

    def test_dropped_via_rule_duplicate_is_caught(self):
        right = copy.deepcopy(self.left)
        right["via_padstacks"] = ["V1"]  # upstream writes it twice
        group = self._compare(right).group("via_rules")
        self.assertEqual([d.op for d in group.differences], ["~"])

    def test_via_info_clearance_class_change_is_caught(self):
        right = copy.deepcopy(self.left)
        right["via_infos"][0]["clearance_class"] = "via"
        self.assertFalse(self._compare(right).ok)

    def test_schema_mismatch_is_an_error_not_a_pass(self):
        right = copy.deepcopy(self.left)
        right["schema"] = "something/else"
        result = self._compare(right)
        self.assertFalse(result.ok)
        self.assertTrue(result.errors)


# ==========================================================================
# 2. Comparator: blind exactly where the exclusions say
# ==========================================================================


class ComparatorExclusionTests(unittest.TestCase):
    def setUp(self):
        self.left = mini_doc()

    def _clean(self, right: dict, msg: str = "") -> gc.CompareResult:
        result = gc.compare(self.left, right, fixture="mini")
        self.assertTrue(result.ok, f"{msg}\n{gc.render_report(result, verbose=True)}")
        return result

    def test_ordering_of_unordered_collections_is_ignored(self):
        rng = random.Random(20260828)
        right = copy.deepcopy(self.left)
        for key in ("padstacks", "images", "components", "nets", "net_classes",
                    "via_infos", "via_rules", "structure_areas", "boundary"):
            rng.shuffle(right[key])
        rng.shuffle(right["via_padstacks"])
        rng.shuffle(right["rules"]["clearance_matrix"])
        rng.shuffle(right["wiring"]["by_kind_fixed"])
        for net in right["nets"]:
            rng.shuffle(net["pins"])
        for comp in right["components"]:
            rng.shuffle(comp["pins"])
            rng.shuffle(comp["keepouts"])
        for pad in right["padstacks"]:
            rng.shuffle(pad["shapes"])
        for image in right["images"]:
            rng.shuffle(image["pins"])
        for cls in right["net_classes"]:
            rng.shuffle(cls["nets"])
            rng.shuffle(cls["use_layer"])
        self._clean(right, "shuffling order must not produce differences")

    def test_layer_order_is_not_ignored_even_though_other_order_is(self):
        right = copy.deepcopy(self.left)
        right["layers"].reverse()
        self.assertFalse(gc.compare(self.left, right).ok)

    def test_float_formatting_noise_is_ignored(self):
        right = json.loads(json.dumps(self.left))

        def jitter(node):
            if isinstance(node, dict):
                for key, value in node.items():
                    node[key] = jitter(value)
                return node
            if isinstance(node, list):
                return [jitter(v) for v in node]
            if isinstance(node, float):
                return node + node * 1e-13 + 1e-13
            return node

        jitter(right)
        self._clean(right, "1e-13 relative jitter must not produce differences")

    def test_a_real_geometry_change_survives_the_float_normalisation(self):
        right = copy.deepcopy(self.left)
        right["padstacks"][0]["shapes"][0]["coords"][0] -= 0.001
        self.assertFalse(gc.compare(self.left, right).ok)

    def test_class_pair_spelling_variants_agree(self):
        # The three spellings of the same diagonal cell, as parsed from DSN.
        variants = [
            "(clearance 50.0 (type smd))",
            "(clearance 50.0 (type smd_smd))",
            "(clearance 50.0 (type smd-smd))",
        ]
        docs = []
        for variant in variants:
            docs.append(
                gc.adapt_java_dump(
                    MINI_DUMP.replace("(clearance 50.0 (type smd))", variant),
                    source="variant",
                )
            )
        for other in docs[1:]:
            result = gc.compare(docs[0], other)
            self.assertTrue(result.ok, gc.render_report(result, verbose=True))

    def test_off_diagonal_cells_are_excluded_and_counted(self):
        right = copy.deepcopy(self.left)
        right["rules"]["clearance_matrix"].append(
            {"classes": ["smd", "via"], "layer": "F.Cu", "value": 77.0}
        )
        result = gc.compare(self.left, right)
        self.assertTrue(result.ok, gc.render_report(result, verbose=True))
        self.assertEqual(result.group("clearance_matrix").excluded, 1)
        self.assertIn("excluded off-diagonal", gc.render_report(result))

    def test_design_name_and_source_are_not_compared(self):
        right = copy.deepcopy(self.left)
        right["design"] = "totally-different"
        right["source"] = "ours.json"
        right["producer"] = "fr-cli"
        self._clean(right, "provenance must not be compared")

    def test_component_rotation_is_compared_at_the_writers_precision(self):
        # `Component.write_scope` writes `(int) Math.round(rotation)`, so the oracle cannot see a
        # fractional degree. Half rounds *up*, towards positive infinity, like `Math.round`.
        right = copy.deepcopy(self.left)
        for comp in right["components"]:
            if comp.get("rotation") is not None:
                comp["rotation"] = comp["rotation"] + 0.5 - 0.001
        self._clean(right, "a sub-degree rotation delta must not be a difference")

    def test_a_whole_degree_rotation_change_is_still_caught(self):
        right = copy.deepcopy(self.left)
        placed = next(c for c in right["components"] if c.get("rotation") is not None)
        placed["rotation"] = placed["rotation"] + 1
        self.assertFalse(gc.compare(self.left, right).ok)

    def test_java_round_is_half_up_not_half_even(self):
        self.assertEqual(gc._java_round(2.5), 3)
        self.assertEqual(gc._java_round(-2.5), -2)  # Python's round() answers -2 too, by luck
        self.assertEqual(gc._java_round(0.5), 1)  # ...but round(0.5) is 0
        self.assertEqual(gc._java_round(1.5), 2)
        self.assertEqual(gc._java_round(-0.5), 0)
        self.assertEqual(gc._java_round(352.5), 353)

    def test_conduction_area_fixed_state_is_folded_away(self):
        # `write_conduction_area_scope` never writes `(type …)`, so a plane inserted SYSTEM_FIXED
        # reads back UNFIXED. Both sides collapse into one bucket.
        right = copy.deepcopy(self.left)
        right["wiring"]["by_kind_fixed"] = [
            {"kind": "conduction_area", "fixed": "system_fixed", "count": 2},
        ]
        left = copy.deepcopy(self.left)
        left["wiring"]["by_kind_fixed"] = [
            {"kind": "conduction_area", "fixed": "unfixed", "count": 2},
        ]
        result = gc.compare(left, right, fixture="mini")
        self.assertTrue(result.ok, gc.render_report(result, verbose=True))

    def test_conduction_area_counts_are_still_compared(self):
        right = copy.deepcopy(self.left)
        right["wiring"]["by_kind_fixed"] = [
            {"kind": "conduction_area", "fixed": "system_fixed", "count": 3},
        ]
        left = copy.deepcopy(self.left)
        left["wiring"]["by_kind_fixed"] = [
            {"kind": "conduction_area", "fixed": "unfixed", "count": 2},
        ]
        self.assertFalse(gc.compare(left, right).ok)

    def test_trace_fixed_state_is_not_folded(self):
        right = copy.deepcopy(self.left)
        right["wiring"]["by_kind_fixed"] = [
            {"kind": "trace", "fixed": "system_fixed", "count": 2},
        ]
        left = copy.deepcopy(self.left)
        left["wiring"]["by_kind_fixed"] = [
            {"kind": "trace", "fixed": "unfixed", "count": 2},
        ]
        self.assertFalse(gc.compare(left, right).ok)

    def test_gate_only_drops_the_extra_groups(self):
        right = copy.deepcopy(self.left)
        right["images"][0]["side"] = "back"
        self.assertFalse(gc.compare(self.left, right).ok)
        self.assertTrue(gc.compare(self.left, right, gate_only=True).ok)
        names = {g.name for g in gc.compare(self.left, right, gate_only=True).groups}
        self.assertEqual(names, gc.GATE_GROUPS)


# ==========================================================================
# 2. Comparator internals
# ==========================================================================


class DifferTests(unittest.TestCase):
    def setUp(self):
        self.differ = gc._Differ(gc.DEFAULT_DECIMALS)

    def test_mapping_reports_all_three_ops(self):
        result = self.differ.mapping("g", "G1.1", {"a": 1, "b": 2}, {"b": 3, "c": 4})
        self.assertEqual(
            sorted((d.op, d.key) for d in result.differences),
            [("+", "c"), ("-", "a"), ("~", "b")],
        )

    def test_mapping_flags_sub_quantum_drift_as_a_note_not_a_difference(self):
        # Straddles the 6-decimal rounding boundary: 1.0 vs 1.000001 after
        # quantisation, but only 7e-7 apart in truth.
        result = self.differ.mapping("g", "G1.1", {"a": 1.0000004}, {"a": 1.0000011})
        self.assertEqual(result.differences, [])
        self.assertEqual(len(result.notes), 1)
        self.assertEqual(result.notes[0].note, "float-noise")

    def test_a_string_difference_is_never_float_noise(self):
        result = self.differ.mapping(
            "g", "G1.1", {"a": {"side": "back", "x": 1.0}}, {"a": {"side": "front", "x": 1.0}}
        )
        self.assertEqual(len(result.differences), 1)
        self.assertEqual(result.notes, [])

    def test_multiset_keeps_multiplicity(self):
        result = self.differ.multiset("g", "G1.1", ["x", "x", "y"], ["x", "y"])
        self.assertEqual([(d.op, d.left, d.right) for d in result.differences], [("~", "x2", "x1")])

    def test_sequence_reports_index_positions(self):
        result = self.differ.sequence("g", "G1.1", [1, 2, 3], [1, 9])
        self.assertEqual([(d.op, d.key) for d in result.differences], [("~", "[1]"), ("-", "[2]")])

    def test_canon_folds_negative_zero(self):
        self.assertEqual(gc._canon(-0.0, 6), 0.0)
        self.assertEqual(gc._key(-0.0, 6), gc._key(0.0, 6))

    def test_bools_are_not_numbers(self):
        result = self.differ.mapping("g", "G1.1", {"a": True}, {"a": False})
        self.assertEqual(len(result.differences), 1)
        self.assertIsNone(result.differences[0].note)


class ValidationTests(unittest.TestCase):
    def test_clean_document_has_no_problems(self):
        self.assertEqual(gc.validate_document(mini_doc()), [])

    def test_missing_keys_are_reported(self):
        self.assertTrue(gc.validate_document({"schema": gc.SCHEMA_ID}))

    def test_bad_layer_type_is_reported(self):
        doc = mini_doc()
        doc["layers"][0]["type"] = "conductor"
        self.assertTrue(any("layers[0].type" in p for p in gc.validate_document(doc)))

    def test_bad_fixed_state_spelling_is_reported(self):
        doc = mini_doc()
        doc["wiring"]["by_kind_fixed"][0]["fixed"] = "fix"
        self.assertTrue(any("fixed" in p for p in gc.validate_document(doc)))

    def test_bad_clearance_pair_arity_is_reported(self):
        doc = mini_doc()
        doc["rules"]["clearance_matrix"][0]["classes"] = ["only-one"]
        self.assertTrue(any("classes" in p for p in gc.validate_document(doc)))

    def test_pin_reference_shape_is_checked(self):
        doc = mini_doc()
        doc["nets"][0]["pins"] = ["U-1-1"]
        self.assertTrue(any("pins" in p for p in gc.validate_document(doc)))


class LoaderTests(unittest.TestCase):
    def test_fixture_name_strips_the_dump_suffix(self):
        self.assertEqual(gc.fixture_name("/x/Issue026-J2_reference.import.dsn"), "Issue026-J2_reference")
        self.assertEqual(gc.fixture_name("/x/Issue026-J2_reference.json"), "Issue026-J2_reference")

    def test_manifest_lists_more_fixtures_than_there_are_dumps(self):
        # The gap is the boards Java itself cannot import; the sweep names them.
        expected = gc.expected_fixtures()
        self.assertGreaterEqual(len(expected), len(dump_paths()))


# ==========================================================================
# 3. Fixture sweep — the G1.0 harness gate
# ==========================================================================


@unittest.skipUnless(dump_paths(), f"no golden dumps in {DUMP_DIR}")
class GoldenDumpSweepTests(unittest.TestCase):
    """Every checked-in dump: parses, validates, round-trips, self-compares."""

    @classmethod
    def setUpClass(cls):
        cls.paths = dump_paths()
        cls.docs = {gc.fixture_name(p): gc.parse_java_dump(p) for p in cls.paths}

    def test_every_dump_validates(self):
        for name, doc in self.docs.items():
            with self.subTest(fixture=name):
                self.assertEqual(gc.validate_document(doc), [])

    def test_every_dump_self_compares_clean_through_json(self):
        for name, doc in self.docs.items():
            with self.subTest(fixture=name):
                other = json.loads(json.dumps(doc, ensure_ascii=False))
                result = gc.compare(doc, other, fixture=name)
                self.assertTrue(result.ok, gc.render_report(result, verbose=True))
                self.assertEqual(result.diff_count, 0)

    def test_sweep_is_not_vacuous(self):
        """A parser that returned empty documents would pass every self-compare."""
        totals = {
            key: sum(gc.document_stats(doc)[key] for doc in self.docs.values())
            for key in ("layers", "clearance", "padstacks", "components", "nets", "net_pins", "wiring", "via_rules")
        }
        # Floors sit ~5 % under the 2026-08-28 checked-in dumps, so a real
        # regeneration does not trip them but a parser that starts dropping
        # whole scopes does.
        self.assertGreaterEqual(totals["layers"], 90)  # 43 boards, >= 2 layers each
        self.assertGreaterEqual(totals["clearance"], 580)
        self.assertGreaterEqual(totals["padstacks"], 690)
        self.assertGreaterEqual(totals["components"], 2900)
        self.assertGreaterEqual(totals["nets"], 6300)
        self.assertGreaterEqual(totals["net_pins"], 18500)
        self.assertGreaterEqual(totals["wiring"], 16300)
        self.assertGreaterEqual(totals["via_rules"], 1150)

    def test_oracle_read_failures_are_detected_from_the_sibling_log(self):
        """The two fixtures whose own Java run did not finish, and only those."""
        failed = {
            gc.fixture_name(p): gc.oracle_read_failure(p)
            for p in self.paths
            if gc.oracle_read_failure(p)
        }
        self.assertEqual(
            set(failed),
            {"Issue006-LPC18XX_43XX_SCH", "Issue229-display-8-digit-hc595"},
            "the ORACLE-FAILED set changed; re-read the logs before touching the markers",
        )
        for marker in failed.values():
            self.assertIn(marker, gc.ORACLE_READ_FAILURE_MARKERS)

    def test_a_dump_without_a_log_is_not_reported_as_failed(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "Nolog.import.dsn")
            with open(path, "w", encoding="utf-8") as handle:
                handle.write(MINI_DUMP)
            self.assertIsNone(gc.oracle_read_failure(path))

    def test_every_board_declares_layers_and_a_default_clearance(self):
        for name, doc in self.docs.items():
            with self.subTest(fixture=name):
                self.assertGreaterEqual(len(doc["layers"]), 1)
                self.assertEqual(doc["resolution"]["unit"], doc["unit"])
                self.assertGreater(doc["resolution"]["value"], 0)
                matrix, _ = gc._matrix_map(doc)
                for layer in doc["layers"]:
                    self.assertIn(f"(default,default)@{layer['name']}", matrix)

    def test_every_pin_reference_names_a_placed_component(self):
        """Catches a tokenizer that silently truncates `"COMP"-"PIN"` refs."""
        for name, doc in self.docs.items():
            with self.subTest(fixture=name):
                components = {c["name"] for c in doc["components"]}
                if not components:
                    continue
                for net in doc["nets"]:
                    for comp, pin in net["pins"]:
                        self.assertIn(comp, components, f"{name}: net {net['name']}")
                        self.assertNotEqual(pin, "")

    def test_every_component_pin_exists_in_its_image(self):
        for name, doc in self.docs.items():
            with self.subTest(fixture=name):
                images = {i["name"]: {p["name"] for p in i["pins"]} for i in doc["images"]}
                for comp in doc["components"]:
                    known = images.get(comp["image"])
                    if known is None:
                        continue
                    for pin in comp["pins"]:
                        self.assertIn(pin["name"], known, f"{name}: {comp['name']}")

    def test_every_padstack_referenced_by_an_image_exists(self):
        for name, doc in self.docs.items():
            with self.subTest(fixture=name):
                padstacks = {p["name"] for p in doc["padstacks"]}
                for image in doc["images"]:
                    for pin in image["pins"]:
                        self.assertIn(pin["padstack"], padstacks, f"{name}: {image['name']}")

    def test_no_dump_contains_marker_characters(self):
        for path in self.paths:
            with self.subTest(fixture=gc.fixture_name(path)):
                with open(path, "r", encoding="utf-8", errors="replace") as handle:
                    text = handle.read()
                self.assertNotIn("\x01", text)
                self.assertNotIn("\x02", text)

    def test_a_perturbed_dump_is_reported_as_a_mismatch(self):
        """The sweep is only meaningful if a real divergence would be caught."""
        name = "Issue026-J2_reference"
        doc = self.docs.get(name) or next(iter(self.docs.values()))
        perturbed = copy.deepcopy(doc)
        perturbed["rules"]["clearance_matrix"][0]["value"] += 1.0
        if perturbed["components"]:
            perturbed["components"][0]["rotation"] = (
                (perturbed["components"][0]["rotation"] or 0) + 90
            )
        result = gc.compare(doc, perturbed, fixture=name)
        self.assertFalse(result.ok)
        self.assertGreaterEqual(result.diff_count, 1)

    def test_absent_dumps_are_the_three_java_cannot_import(self):
        present = set(self.docs)
        absent = [n for n in gc.expected_fixtures() if n not in present]
        self.assertEqual(
            sorted(absent),
            [
                "Issue029-hw48na",
                "Issue107-freq_teiler_200kHz_kicad",
                "Issue107-freq_teiler_200kHz_kicad_bad",
            ],
        )


class CliTests(unittest.TestCase):
    def test_pair_mode_self_comparison_exits_zero(self):
        paths = dump_paths()
        if not paths:
            self.skipTest("no golden dumps")
        import contextlib
        import io

        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            code = gc.main(["--java-dump", paths[0], "--ours", paths[0]])
        self.assertEqual(code, 0)
        self.assertIn("CLEAN", buf.getvalue())

    def test_exclusions_are_documented_and_printable(self):
        import contextlib
        import io

        self.assertGreaterEqual(len(gc.EXCLUSIONS), 5)
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.assertEqual(gc.main(["--exclusions"]), 0)
        self.assertIn("class-pair spelling", buf.getvalue())

    def test_no_arguments_prints_help_and_fails(self):
        import contextlib
        import io

        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.assertEqual(gc.main([]), 2)
        self.assertIn("--java-dump", buf.getvalue())


if __name__ == "__main__":
    unittest.main(verbosity=2)
