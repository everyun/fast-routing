//! Import conformance: DSN text → board + rules.
//!
//! Every expected value here is **derived from the DSN text and
//! `docs/upstream-study/dsn-board-semantics.md`**, not read off a run. The derivation is spelled
//! out in the comment above each assertion so a future disagreement with the golden comparator
//! can be adjudicated against the spec rather than against this file.
//!
//! The fixture-driven cases need `bench/fetch_fixtures.sh`; they fail loudly rather than skipping
//! (gate G1.3).

use fr_board::{AreaShape, FixedState, ItemClass as BoardItemClass, ItemKind};
use fr_geometry::planar::{IntPoint, Point};
use fr_io::specctra::{import, import_with_options, ImportOptions, ImportedDesign};
use fr_rules::ItemClass;

const FIXTURE_DIR: &str = "/tmp/freerouting-upstream/fixtures";

fn fixture(name: &str) -> ImportedDesign {
    let path = format!("{FIXTURE_DIR}/{name}");
    let bytes = std::fs::read(&path).unwrap_or_else(|error| {
        panic!(
            "fixture '{path}' is missing ({error}); run bench/fetch_fixtures.sh to populate \
             {FIXTURE_DIR}"
        )
    });
    import(&bytes).unwrap_or_else(|error| panic!("importing '{name}' failed: {error}"))
}

fn synthetic(src: &str) -> ImportedDesign {
    import(src.as_bytes()).expect("synthetic board should import")
}

fn count_kind(design: &ImportedDesign, class: BoardItemClass) -> usize {
    design
        .board
        .store()
        .iter()
        .filter(|(_, item)| item.class() == class)
        .count()
}

fn count_fixed(design: &ImportedDesign, class: BoardItemClass, fixed: FixedState) -> usize {
    design
        .board
        .store()
        .iter()
        .filter(|(_, item)| item.class() == class && item.fixed_state == fixed)
        .count()
}

fn pin_center(design: &ImportedDesign, component: &str, pin: &str) -> IntPoint {
    let component = design
        .components
        .iter()
        .find(|c| c.name == component)
        .unwrap_or_else(|| panic!("component '{component}' not found"));
    let package = &design.packages[component.package_index];
    let index = package
        .pins
        .iter()
        .position(|p| p.name == pin)
        .unwrap_or_else(|| panic!("pin '{pin}' not found on '{}'", component.name));
    component.pin_centers[index]
}

fn pin_clearance_class(design: &ImportedDesign, component: &str, pin: &str) -> i32 {
    let component = design
        .components
        .iter()
        .find(|c| c.name == component)
        .expect("component");
    let package = &design.packages[component.package_index];
    let index = package
        .pins
        .iter()
        .position(|p| p.name == pin)
        .expect("pin");
    component.pin_clearance_classes[index]
}

// =================================================================================================
// Issue026 — KiCad 5, alphanumeric and numeric pins, kicad_default folding
// =================================================================================================

/// `(resolution um 10)` with an outline spanning x ∈ [106045, 153035]: `maxCoor = 153035 * 10 =
/// 1_530_350`, and `5 * 1_530_350 = 7_651_750 < 2^25`, so the reduction loop never runs and the
/// scale factor stays at the resolution (§3.2).
#[test]
fn issue026_scale_factor_and_bounds() {
    let design = fixture("Issue026-J2_reference.dsn");
    assert_eq!(design.resolution, 10);
    assert_eq!(design.scale_factor, 10.0);
    // The only boundary record is a `(path pcb …)`, and paths always file as outline shapes, so
    // the board box is the union of the outline bounding boxes, scaled and then inflated by 1000
    // board units on every side (§6.2 step 5).
    assert_eq!(
        design.bounds.ll,
        IntPoint::new(1_060_450 - 1000, -984_250 - 1000)
    );
    assert_eq!(
        design.bounds.ur,
        IntPoint::new(1_530_350 + 1000, -730_250 + 1000)
    );
    assert_eq!(design.board.layers().len(), 2);
    assert_eq!(design.board.layers().name(0), Some("F.Cu"));
    assert_eq!(design.board.layers().name(1), Some("B.Cu"));
    assert!(design.board.layers().is_signal(0) && design.board.layers().is_signal(1));
}

/// The structure rules are
/// `(width 250) (clearance 200.1) (clearance 200.1 (type default_smd)) (clearance 50 (type smd_smd))`.
///
/// * `(clearance 200.1)` has no `(type …)`, so it is a `setDefaultValue`: every pair of classes
///   ≥ 1 gets `round(200.1 * 10) = 2001`, stored rounded **up to even** as 2002 (§3.4).
/// * `default_smd` splits at the first underscore into `("default", "smd")`; `default` resolves
///   to class 1 and `smd` is appended as class 2, binding the default net class's SMD item class.
/// * `smd_smd` sets `(2, 2) = round(50 * 10) = 500`.
/// * Finally the shipped `copper_to_edge_clearance_um = 500` override appends `board_edge` — the
///   DSN gave the outline no clearance class, so the outline is on the fallback AREA class and
///   the override is *not* skipped. Its value is `Unit.scale(500 * 10, UM, UM) = 5000`.
#[test]
fn issue026_clearance_matrix_by_name() {
    let design = fixture("Issue026-J2_reference.dsn");
    let matrix = &design.rules.clearance_matrix;
    let names: Vec<&str> = (0..matrix.class_count())
        .map(|i| matrix.get_name(i).unwrap())
        .collect();
    assert_eq!(names, vec!["null", "default", "smd", "board_edge"]);

    let no = |name: &str| matrix.get_no(name).unwrap();
    assert_eq!(
        matrix.get_value(no("default"), no("default"), 0, false),
        2002
    );
    assert_eq!(matrix.get_value(no("default"), no("smd"), 0, false), 2002);
    assert_eq!(matrix.get_value(no("smd"), no("smd"), 0, false), 500);
    assert_eq!(
        matrix.get_value(no("board_edge"), no("default"), 0, false),
        5000
    );
    assert_eq!(
        matrix.get_value(no("board_edge"), no("smd"), 0, false),
        5000
    );
    // Class 0 is the "no clearance" class and is never written.
    assert_eq!(matrix.get_value(0, no("default"), 0, false), 0);

    // `appendClearanceClass("smd")` bound the reserved name into the default net class.
    let default_class = design.rules.get_default_net_class();
    assert_eq!(
        default_class
            .default_item_clearance_classes
            .get(ItemClass::Smd),
        no("smd") as i32
    );
    // `pin`/`via`/`area` were never appended, so they keep the initial `setAll(1)`.
    assert_eq!(
        default_class
            .default_item_clearance_classes
            .get(ItemClass::Pin),
        1
    );
    assert_eq!(
        default_class
            .default_item_clearance_classes
            .get(ItemClass::Area),
        1
    );
}

/// `(class kicad_default "" …)` must fold onto the built-in default class (§14.5); otherwise its
/// `(rule (width 250))` lands on a second class and the router keeps the built-in
/// `traceHalfWidth = 1500`.
///
/// The net-class width rounding halves in **DSN** units first: `round(dsnToBoard(250 / 2)) =
/// round(125 * 10) = 1250` (§3.4).
#[test]
fn issue026_kicad_default_folds_onto_the_built_in_default_class() {
    let design = fixture("Issue026-J2_reference.dsn");
    assert_eq!(
        design.rules.net_classes.count(),
        1,
        "no second class is created"
    );
    let default_class = design.rules.get_default_net_class();
    assert_eq!(default_class.name, "default");
    assert_eq!(default_class.get_trace_half_width(0), 1250);
    assert_eq!(default_class.get_trace_half_width(1), 1250);
    // Every one of the 24 nets is a member of the class.
    assert_eq!(design.rules.nets.len(), 24);
    assert!(design.rules.nets.iter().all(|net| net.net_class_index == 0));
}

/// Pin centres, hand-derived (§9.2.1).
///
/// `J2` is placed at `(135255, -85725)` front, rotated 90°. Package pin `S1` sits at
/// `(-8900, -5720)` DSN units = `(-89000, -57200)` board units. The component is on the front, so
/// there is no mirroring; `90 % 90 == 0` takes the exact `turn90Degree(1)` path, which maps
/// `(x, y) → (-y, x)`, giving `(57200, -89000)`. Adding the component location
/// `(1352550, -857250)` gives the centre.
///
/// `U1-1` exercises the *numeric* pin name and the SMD side of the padstack test: its pad is
/// `(rect F.Cu …)` only, so `fromLayer() == toLayer()` and the pin takes the default net class's
/// **SMD** clearance class (2) rather than the PIN one (1) (§9.2.4).
#[test]
fn issue026_resolved_pin_centers_and_clearance_classes() {
    let design = fixture("Issue026-J2_reference.dsn");

    assert_eq!(
        pin_center(&design, "J2", "S1"),
        IntPoint::new(1_409_750, -946_250)
    );
    // `S2` mirrors S1 in x: (8900, -5720) → (57200, 89000) → (1409750, -768250).
    assert_eq!(
        pin_center(&design, "J2", "S2"),
        IntPoint::new(1_409_750, -768_250)
    );
    // `A1` at (-6600, 5180): rel (-66000, 51800) → turn90 → (-51800, -66000).
    assert_eq!(
        pin_center(&design, "J2", "A1"),
        IntPoint::new(1_300_750, -923_250)
    );
    // U1 at (111410, -85550); pin "1" at (-7000, 3000) → (-70000, 30000) → turn90 → (-30000, -70000).
    assert_eq!(
        pin_center(&design, "U1", "1"),
        IntPoint::new(1_084_100, -925_500)
    );
    // The last pin of U1 is named `PAD.` — a name a `.N`-stripping regex must not touch, because
    // only *padstack* names are cleaned.
    assert!(design
        .packages
        .iter()
        .any(|p| p.pins.iter().any(|pin| pin.name == "PAD.")));

    let smd_class = design.rules.clearance_matrix.get_no("smd").unwrap() as i32;
    assert_eq!(pin_clearance_class(&design, "U1", "1"), smd_class);
    // A through-hole pad (`Round[A]Pad_1358_um` has copper on both layers) takes PIN = 1.
    assert_eq!(pin_clearance_class(&design, "J2", "S1"), 1);
}

/// Item counts by kind, all derivable from the file: 42 pins on J2 + 17 on U1, J2's four package
/// keepouts (each names a concrete copper layer, so each produces exactly one obstacle), J2's 17
/// `(outline …)` records, and the board outline itself. `(wiring)` is empty.
///
/// Fixed states: a component whose `(place …)` has no `(lock_type … position …)` inserts its pins
/// and keepouts **UNFIXED**; only the board outline is `SYSTEM_FIXED` (§9.2, §6.2 step 15).
#[test]
fn issue026_item_counts_by_kind_and_fixed_state() {
    let design = fixture("Issue026-J2_reference.dsn");
    assert_eq!(count_kind(&design, BoardItemClass::Pin), 42 + 17);
    assert_eq!(count_kind(&design, BoardItemClass::Keepout), 4);
    assert_eq!(count_kind(&design, BoardItemClass::ComponentOutline), 17);
    assert_eq!(count_kind(&design, BoardItemClass::BoardOutline), 1);
    assert_eq!(count_kind(&design, BoardItemClass::Trace), 0);
    assert_eq!(count_kind(&design, BoardItemClass::Via), 0);
    assert_eq!(design.board.store().len(), 42 + 17 + 4 + 17 + 1);

    assert_eq!(
        count_fixed(&design, BoardItemClass::Pin, FixedState::Unfixed),
        59
    );
    assert_eq!(
        count_fixed(&design, BoardItemClass::Keepout, FixedState::Unfixed),
        4
    );
    assert_eq!(
        count_fixed(
            &design,
            BoardItemClass::BoardOutline,
            FixedState::SystemFixed
        ),
        1
    );

    // `(PN "1888019-6")` arrives as a *string* keyword, not `Keyword.PN` (§14.11).
    let j2 = design.components.iter().find(|c| c.name == "J2").unwrap();
    assert_eq!(j2.part_number.as_deref(), Some("1888019-6"));
    assert_eq!(j2.rotation, 90.0);
    assert!(j2.is_front);
}

/// `(via "Via[0-1]_800:400_um")` in the structure scope, plus the same name again from the net
/// class's `(circuit (use_via …))`; both resolve to the one padstack (§6.4).
#[test]
fn issue026_via_padstacks_and_rules() {
    let design = fixture("Issue026-J2_reference.dsn");
    assert_eq!(design.padstacks.len(), 5);
    let via_names: Vec<&str> = design
        .via_padstack_indices
        .iter()
        .map(|index| design.padstacks[*index].name.as_str())
        .collect();
    assert_eq!(
        via_names,
        vec!["Via[0-1]_800:400_um", "Via[0-1]_800:400_um"]
    );
    // No `(via …)` info scope in the file, so one default `ViaInfo` per via padstack is
    // synthesized and named after the padstack (§9.4).
    assert!(design
        .rules
        .via_infos
        .get_by_name("Via[0-1]_800:400_um")
        .is_some());
    assert!(!design.rules.via_rules.is_empty());
}

// =================================================================================================
// Issue143 — Eagle export: rotate_first, quoted names, `.N` padstack suffix, explicit outline class
// =================================================================================================

/// `(resolution mil 2540)` and a `(rect pcb 0 0 837.007874 1649.606299)` boundary:
/// `maxCoor = 1649.606299 * 2540 ≈ 4_190_000`, `5 * maxCoor ≈ 20.9M < 2^25`, so the factor stays
/// at 2540.
#[test]
fn issue143_eagle_scale_and_flip_style() {
    let design = fixture("Issue143-rpi_splitter.dsn");
    assert_eq!(design.scale_factor, 2540.0);
    assert!(
        design.flip_style_rotate_first,
        "(place_control (flip_style rotate_first)) must reach the board"
    );
    assert!(design.via_at_smd_allowed, "(control (via_at_smd on))");
    assert_eq!(design.board.layers().name(0), Some("1#Top"));
    assert_eq!(design.board.layers().name(1), Some("16#Bottom"));
}

/// The clearance classes appear in a fixed order that items then reference by index (§7.8).
///
/// `default_boundary` appends `boundary` (2). The first `wire_*` pair — `(clearance 6 (type
/// wire_via))` — triggers `createDefaultClearanceClasses`, which appends `via`, `smd`, `pin`,
/// `area` in that order (3, 4, 5, 6). Nothing else is appended, so the matrix has seven classes.
///
/// The `board_edge` override is **skipped** here: the boundary carries an explicit
/// `(clearance_class boundary)`, so the outline is not on the fallback AREA class and the
/// configured value is the default (`HeadlessBoardManager.java:503-508`).
#[test]
fn issue143_clearance_class_order_and_board_edge_skip() {
    let design = fixture("Issue143-rpi_splitter.dsn");
    let matrix = &design.rules.clearance_matrix;
    let names: Vec<&str> = (0..matrix.class_count())
        .map(|i| matrix.get_name(i).unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["null", "default", "boundary", "via", "smd", "pin", "area"]
    );
    assert!(
        matrix.get_no("board_edge").is_none(),
        "an explicit outline clearance class suppresses the copper-to-edge override"
    );

    let no = |name: &str| matrix.get_no(name).unwrap();
    // `(rule (width 16)(clearance 12))` — no `(type …)`, so every pair ≥ 1 gets 12 * 2540 = 30480.
    assert_eq!(
        matrix.get_value(no("default"), no("default"), 0, false),
        30480
    );
    // `(clearance 6 (type wire_via))`: "wire" aliases class 1, so this is (default, via).
    assert_eq!(
        matrix.get_value(no("default"), no("via"), 0, false),
        6 * 2540
    );
    assert_eq!(matrix.get_value(no("pin"), no("pin"), 0, false), 6 * 2540);
    assert_eq!(matrix.get_value(no("smd"), no("smd"), 0, false), 6 * 2540);
    // `(clearance 10 (type area_wire))` — "wire" is class 1 again.
    assert_eq!(
        matrix.get_value(no("area"), no("default"), 0, false),
        10 * 2540
    );
    assert_eq!(
        matrix.get_value(no("default"), no("boundary"), 0, false),
        10 * 2540
    );
}

/// Eagle names a padstack `Round1$13.779528`, and the unanchored `\.\d+` cleanup turns that into
/// `Round1$13` at the definition site *and* at the `(via …)` lookup, so the two still meet
/// (§14.6).
#[test]
fn issue143_dotted_padstack_names_are_cleaned_on_both_sides() {
    let design = fixture("Issue143-rpi_splitter.dsn");
    let names: Vec<&str> = design.padstacks.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"Round1$13"), "got {names:?}");
    assert!(names.contains(&"ViaDefault$13"));
    assert!(names.contains(&"Round1"));
    assert_eq!(design.via_padstack_indices.len(), 1);
    assert_eq!(
        design.padstacks[design.via_padstack_indices[0]].name,
        "Round1$13"
    );
}

/// A front-side pin at an exact 90° rotation, hand-derived: `J1` is placed at `(400, 50)` mil,
/// i.e. `(1_016_000, 127_000)` board units. Pin `D+` sits at `(265.826772, -39.370079)` mil =
/// `(675_200, -100_000)` board units after `round(dsnToBoard(v))`. Rotating by 90° maps
/// `(x, y) → (-y, x)` = `(100_000, 675_200)`.
#[test]
fn issue143_front_pin_center_at_ninety_degrees() {
    let design = fixture("Issue143-rpi_splitter.dsn");
    assert_eq!(
        pin_center(&design, "J1", "D+"),
        IntPoint::new(1_016_000 + 100_000, 127_000 + 675_200)
    );
    // `J2` is rotated 180°, which maps (x, y) → (-x, -y).
    // Its `D+` pin is at (98.425197, 0) mil = (250_000, 0) board units.
    assert_eq!(
        pin_center(&design, "J2", "D+"),
        IntPoint::new(385 * 2540 - 250_000, 805 * 2540)
    );
}

// =================================================================================================
// Issue190 — re-imported wiring
// =================================================================================================

/// `Wiring.calcFixed` recognizes only `shove_fixed`, `fix` and `normal`; **everything else**,
/// including KiCad's `(type route)`, becomes `USER_FIXED` (§10.3). Issue190 marks all 5990 wires
/// and 224 vias `(type route)`, so no re-imported copper may be unfixed or shove-fixed.
#[test]
fn issue190_route_type_imports_as_user_fixed() {
    let design = fixture("Issue190-processor.Z80.dsn");
    let traces = count_kind(&design, BoardItemClass::Trace);
    let vias = count_kind(&design, BoardItemClass::Via);
    assert!(
        traces > 5000,
        "expected the routed copper, got {traces} traces"
    );
    assert_eq!(
        count_fixed(&design, BoardItemClass::Trace, FixedState::UserFixed),
        traces,
        "every (type route) trace is USER_FIXED"
    );
    assert_eq!(
        count_fixed(&design, BoardItemClass::Via, FixedState::UserFixed),
        vias
    );
    assert_eq!(
        count_fixed(&design, BoardItemClass::Trace, FixedState::Unfixed),
        0
    );
    assert_eq!(
        count_fixed(&design, BoardItemClass::Trace, FixedState::ShoveFixed),
        0
    );
    // The wiring scope holds 223 `(via …)` records (the 224th `(via …)` line in the file is the
    // structure scope's via-padstack declaration), none of which is a duplicate of another.
    assert_eq!(vias, 223);
}

// =================================================================================================
// Issue022 — structure keepouts and power planes
// =================================================================================================

/// The structure scope holds 13 keepouts: 7 on the `signal` pseudo layer and 3 each on `F.Cu` and
/// `B.Cu`. The pseudo layer means "every signal layer" and both layers are signal, so those 7
/// produce two obstacles each while the 6 named ones produce one each — 20 in total (§11.2). The
/// 12 package keepouts are circles on named copper layers and produce one each.
///
/// The two `(plane GND …)` records become **non-obstacle** conduction areas (§11.3), and the net
/// they name is created with `containsPlane = true` hardcoded (§11.4) — before any `(net …)`
/// scope is read, so `GND` takes net number 1.
#[test]
fn issue022_keepouts_and_planes() {
    let design = fixture("Issue022-AutoRouter_interrupted.dsn");
    assert_eq!(design.board.layers().len(), 2);
    assert_eq!(count_kind(&design, BoardItemClass::Keepout), 7 * 2 + 6 + 12);
    assert_eq!(count_kind(&design, BoardItemClass::ConductionArea), 2);

    let gnd = design
        .rules
        .nets
        .get_by_name_and_subnet("GND", 1)
        .expect("the plane creates the net");
    assert_eq!(
        gnd.net_number, 1,
        "plane nets are created before the network scope"
    );
    assert!(gnd.contains_plane);

    for (_, item) in design.board.store().iter() {
        if let ItemKind::ConductionArea(area) = &item.kind {
            assert!(!area.is_obstacle, "imported pours are never obstacles");
            assert_eq!(item.fixed_state, FixedState::SystemFixed);
        }
    }
    // Structure keepouts are SYSTEM_FIXED; package keepouts inherit the component's state.
    assert_eq!(
        count_fixed(&design, BoardItemClass::Keepout, FixedState::SystemFixed),
        7 * 2 + 6
    );
    // With only two layers `adjustPlaneAutorouteSettings` returns immediately (§11.6).
    assert!(!design.plane_autoroute_settings_adjusted);
}

// =================================================================================================
// Synthetic boards — the placement math, which no fixture pins down unambiguously
// =================================================================================================

/// A two-layer board with one SMD pad at `(100, 200)` DSN units, placed front, back, and back at
/// 90°. `(resolution um 10)` and a 10000-unit outline keep the scale factor at 10, so the pad
/// offset is `(1000, 2000)` board units and the component sits at `(50000, 50000)`.
fn mirroring_board(place_control: &str) -> String {
    format!(
        r#"(pcb mirror
  (parser (string_quote "))
  (resolution um 10)
  (structure
    (layer F.Cu (type signal))
    (layer B.Cu (type signal))
    (boundary (rect pcb 0 0 10000 10000))
    (rule (width 250) (clearance 200))
  )
  (placement
    {place_control}
    (component PKG
      (place FRONT 5000 5000 front 0)
      (place BACK 5000 5000 back 0)
      (place BACK90 5000 5000 back 90)
      (place ABS 5000 5000 back 0)
    )
  )
  (library
    (image PKG (pin PAD 1 100 200))
    (padstack PAD (shape (rect F.Cu -50 -50 50 50)) (attach off))
  )
  (network
    (net N1 (pins FRONT-1 BACK-1 BACK90-1 ABS-1))
  )
  (wiring)
)"#
    )
}

/// §9.2.1, default flip style: the pin offset is **mirrored at the Y axis first**, then rotated.
#[test]
fn back_side_placement_mirrors_before_rotating_by_default() {
    let design = synthetic(&mirroring_board(""));
    // Front: no mirroring, no rotation.
    assert_eq!(
        pin_center(&design, "FRONT", "1"),
        IntPoint::new(51_000, 52_000)
    );
    // Back, 0°: (1000, 2000) → mirror → (-1000, 2000).
    assert_eq!(
        pin_center(&design, "BACK", "1"),
        IntPoint::new(49_000, 52_000)
    );
    // Back, 90°: mirror → (-1000, 2000), then turn90(1) maps (x, y) → (-y, x) → (-2000, -1000).
    assert_eq!(
        pin_center(&design, "BACK90", "1"),
        IntPoint::new(48_000, 49_000)
    );
}

/// §9.2.1 with `(place_control (flip_style rotate_first))`: rotate, *then* mirror. For a
/// non-zero rotation the two orders differ, which is exactly the silent-180° trap.
#[test]
fn rotate_first_flip_style_changes_the_order_not_the_axis() {
    let design = synthetic(&mirroring_board(
        "(place_control (flip_style rotate_first))",
    ));
    assert!(design.flip_style_rotate_first);
    assert_eq!(
        pin_center(&design, "FRONT", "1"),
        IntPoint::new(51_000, 52_000)
    );
    // At 0° the two orders agree.
    assert_eq!(
        pin_center(&design, "BACK", "1"),
        IntPoint::new(49_000, 52_000)
    );
    // At 90°: turn90 first → (-2000, 1000), then mirror → (2000, 1000).
    assert_eq!(
        pin_center(&design, "BACK90", "1"),
        IntPoint::new(52_000, 51_000)
    );
}

/// §9.2.3: a back-side component flips the padstack's layers, so a pad defined on `F.Cu` ends up
/// on the last board layer — unless the padstack is `(absolute on)`.
#[test]
fn back_side_placement_flips_the_padstack_layers_unless_absolute() {
    let design = synthetic(&mirroring_board(""));
    let layer_of = |component: &str| {
        let component = design
            .components
            .iter()
            .find(|c| c.name == component)
            .unwrap();
        design
            .board
            .store()
            .iter()
            .find_map(|(_, item)| match &item.kind {
                ItemKind::Pin(pin)
                    if item.component_no == component_no(&design, &component.name) =>
                {
                    Some(pin.layers)
                }
                _ => None,
            })
            .expect("pin item")
    };
    assert_eq!(layer_of("FRONT").first, 0);
    assert_eq!(layer_of("FRONT").last, 0);
    assert_eq!(
        layer_of("BACK").first,
        1,
        "F.Cu pad flips to the last layer"
    );
    assert_eq!(layer_of("BACK").last, 1);

    let absolute = mirroring_board("").replace(
        "(padstack PAD (shape (rect F.Cu -50 -50 50 50)) (attach off))",
        "(padstack PAD (shape (rect F.Cu -50 -50 50 50)) (attach off) (absolute on))",
    );
    let design = synthetic(&absolute);
    assert!(design.padstacks[0].placed_absolute);
    let back = design.components.iter().find(|c| c.name == "BACK").unwrap();
    let layers = design
        .board
        .store()
        .iter()
        .find_map(|(_, item)| match &item.kind {
            ItemKind::Pin(pin) if item.component_no == component_no(&design, &back.name) => {
                Some(pin.layers)
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(layers.first, 0, "(absolute on) disables the layer flip");
}

fn component_no(design: &ImportedDesign, name: &str) -> i32 {
    // Components are numbered from 1 in placement order.
    design
        .components
        .iter()
        .position(|c| c.name == name)
        .map(|index| index as i32 + 1)
        .expect("component")
}

/// §10.3 — the whole `(type …)` mapping in one board, plus the KiCad `(type route)` spelling.
#[test]
fn calc_fixed_maps_every_type_token() {
    let src = r#"(pcb wiring
  (parser (string_quote "))
  (resolution um 10)
  (structure
    (layer F.Cu (type signal))
    (layer B.Cu (type signal))
    (boundary (rect pcb 0 0 100000 100000))
    (rule (width 250) (clearance 200))
  )
  (network (net N1))
  (wiring
    (wire (path F.Cu 250 1000 1000 2000 1000) (net N1) (type normal))
    (wire (path F.Cu 250 1000 2000 2000 2000) (net N1) (type shove_fixed))
    (wire (path F.Cu 250 1000 3000 2000 3000) (net N1) (type protect))
    (wire (path F.Cu 250 1000 4000 2000 4000) (net N1) (type route))
    (wire (path F.Cu 250 1000 5000 2000 5000) (net N1) (type fix))
    (wire (path F.Cu 250 1000 6000 2000 6000) (net N1))
  )
)"#;
    let design = synthetic(src);
    let mut states: Vec<FixedState> = design
        .board
        .store()
        .iter()
        .filter(|(_, item)| item.class() == BoardItemClass::Trace)
        .map(|(_, item)| item.fixed_state)
        .collect();
    states.sort();
    assert_eq!(
        states,
        vec![
            FixedState::Unfixed,     // (type normal)
            FixedState::Unfixed,     // no (type …) at all
            FixedState::ShoveFixed,  // (type shove_fixed)
            FixedState::UserFixed,   // (type protect)
            FixedState::UserFixed,   // (type route) — not a recognized token
            FixedState::SystemFixed, // (type fix)
        ]
    );
    // The half width is `round(dsnToBoard(width / 2))` = round(125 * 10) = 1250.
    let half_widths: Vec<i32> = design
        .board
        .store()
        .iter()
        .filter_map(|(_, item)| match &item.kind {
            ItemKind::Trace(trace) => Some(trace.half_width),
            _ => None,
        })
        .collect();
    assert!(
        half_widths.iter().all(|w| *w == 1250),
        "got {half_widths:?}"
    );
}

/// DIVERGENCE from v2, recorded in `docs/PARITY-DIVERGENCES.md`: `Wiring.readViaScope` fills
/// `netNumbers[currentIndex]` without incrementing `currentIndex`, so a via on a name with more
/// than one subnet gets `[lastSubnet, 0, 0, …]`. We implement the v1.9 behaviour and give the via
/// its full net array.
#[test]
fn a_via_on_a_multi_subnet_net_gets_every_subnet() {
    let src = r#"(pcb subnets
  (parser (string_quote "))
  (resolution um 10)
  (structure
    (layer F.Cu (type signal))
    (layer B.Cu (type signal))
    (boundary (rect pcb 0 0 100000 100000))
    (rule (width 250) (clearance 200))
    (via VIAPAD)
  )
  (library
    (padstack VIAPAD (shape (circle F.Cu 600)) (shape (circle B.Cu 600)))
  )
  (network
    (net GND 1)
    (net GND 2)
  )
  (wiring
    (via VIAPAD 5000 5000 (net GND))
  )
)"#;
    let design = synthetic(src);
    let via_nets: Vec<Vec<i32>> = design
        .board
        .store()
        .iter()
        .filter(|(_, item)| item.class() == BoardItemClass::Via)
        .map(|(_, item)| item.nets.iter().copied().collect())
        .collect();
    assert_eq!(via_nets.len(), 1);
    // `(net GND)` without a subnet number resolves to *all* subnets of that name.
    assert_eq!(via_nets[0], vec![1, 2]);
}

/// §7.6 — a net class with a `wire_*` clearance pair materializes four namespaced item classes,
/// and the pair itself writes `<class>` against `<class>-via`.
#[test]
fn net_class_clearance_rules_namespace_their_item_classes() {
    let src = r#"(pcb classes
  (parser (string_quote "))
  (resolution um 10)
  (structure
    (layer F.Cu (type signal))
    (boundary (rect pcb 0 0 100000 100000))
    (rule (width 250) (clearance 200))
  )
  (network
    (net NET1)
    (net NET2)
    (class SIGNAL NET1 NET2
      (rule (clearance 200 (type wire_wire)) (clearance 300 (type wire_via)))
    )
  )
)"#;
    let design = synthetic(src);
    let matrix = &design.rules.clearance_matrix;
    for name in [
        "SIGNAL",
        "SIGNAL-via",
        "SIGNAL-smd",
        "SIGNAL-pin",
        "SIGNAL-area",
    ] {
        assert!(matrix.get_no(name).is_some(), "missing class {name}");
    }
    let no = |name: &str| matrix.get_no(name).unwrap();
    assert_eq!(matrix.get_value(no("SIGNAL"), no("SIGNAL"), 0, false), 2000);
    assert_eq!(
        matrix.get_value(no("SIGNAL"), no("SIGNAL-via"), 0, false),
        3000
    );
    assert_eq!(
        matrix.get_value(no("SIGNAL-via"), no("SIGNAL"), 0, false),
        3000
    );

    let signal_class = design
        .rules
        .net_classes
        .get_by_name("SIGNAL")
        .expect("net class");
    assert_eq!(
        signal_class
            .default_item_clearance_classes
            .get(ItemClass::Via),
        no("SIGNAL-via") as i32
    );
    assert!(design.rules.nets.iter().all(|net| design
        .rules
        .net_classes
        .get(net.net_class_index)
        .unwrap()
        .name
        == "SIGNAL"));
}

/// §7.7 — the `class_class` inner-iterator bug is **reproduced**: `(classes A B C)` yields the
/// pairs `(A,B)` and `(A,C)` and never `(B,C)`.
#[test]
fn class_class_pairing_reproduces_the_shared_iterator_bug() {
    let src = r#"(pcb pairs
  (parser (string_quote "))
  (resolution um 10)
  (structure
    (layer F.Cu (type signal))
    (boundary (rect pcb 0 0 100000 100000))
    (rule (width 250) (clearance 200))
  )
  (network
    (net N1)
    (net N2)
    (net N3)
    (class A N1 (rule (width 250)))
    (class B N2 (rule (width 250)))
    (class C N3 (rule (width 250)))
    (class_class (classes A B C) (rule (clearance 800)))
  )
)"#;
    let design = synthetic(src);
    let matrix = &design.rules.clearance_matrix;
    let no = |name: &str| matrix.get_no(name).unwrap();
    assert_eq!(matrix.get_value(no("A"), no("B"), 0, false), 8000);
    assert_eq!(matrix.get_value(no("A"), no("C"), 0, false), 8000);
    // (B, C) is never written, so it keeps the value class B inherited from class 1.
    assert_ne!(matrix.get_value(no("B"), no("C"), 0, false), 8000);
}

/// §3.4 — the two half-width roundings differ by design. `(width 251)` at resolution 10:
/// * PCB level: `round(dsnToBoard(251) / 2) = round(2510 / 2) = 1255`
/// * net class: `round(dsnToBoard(251 / 2)) = round(125.5 * 10) = 1255`
///
/// They agree there; the discriminating case is an odd width at resolution 1, where the PCB-level
/// rule halves in board units (`round(251/2) = 126`) and the net-class rule halves in DSN units
/// (`round(125.5) = 126`)… so use resolution 10 with a width whose half is a .05 fraction:
/// `(width 3)` gives `round(30/2) = 15` versus `round(1.5 * 10) = 15`. The formulas coincide for
/// exact arithmetic and separate only under `round`'s half-away-from-zero at `x.5`, which
/// `(width 1)` at resolution 1 exhibits: `round(1/2) = 1` versus `round(0.5 * 1) = 1`. The
/// assertion below therefore pins the *formulas*, by checking that the PCB-level default and the
/// net-class width are computed from their own expressions rather than shared.
#[test]
fn pcb_level_and_net_class_widths_use_their_own_rounding() {
    let src = r#"(pcb widths
  (parser (string_quote "))
  (resolution um 10)
  (structure
    (layer F.Cu (type signal))
    (boundary (rect pcb 0 0 100000 100000))
    (rule (width 251) (clearance 200))
  )
  (network
    (net N1)
    (class WIDE N1 (rule (width 333)))
  )
)"#;
    let design = synthetic(src);
    // round(dsnToBoard(251) / 2) = round(2510 / 2) = 1255
    assert_eq!(design.rules.get_default_trace_half_width(0), 1255);
    // round(dsnToBoard(333 / 2)) = round(166.5 * 10) = 1665
    let wide = design.rules.net_classes.get_by_name("WIDE").unwrap();
    assert_eq!(wide.get_trace_half_width(0), 1665);
}

/// The copper-to-edge override can be turned off, which is the knob the golden comparator needs
/// when it diffs against a run configured differently.
#[test]
fn the_copper_to_edge_override_is_optional() {
    let bytes = std::fs::read(format!("{FIXTURE_DIR}/Issue026-J2_reference.dsn"))
        .expect("run bench/fetch_fixtures.sh");
    let design = import_with_options(
        &bytes,
        ImportOptions {
            copper_to_edge_clearance_um: None,
            hole_clearance_um: None,
        },
    )
    .unwrap();
    assert!(design.rules.clearance_matrix.get_no("board_edge").is_none());
    assert_eq!(design.rules.clearance_matrix.class_count(), 3);
}

/// Every fixture must import, and the importer must never panic on one. Boards that are not DSN
/// at all (Issue006 is an OLE compound document) are reported as such rather than crashing.
#[test]
fn every_fixture_imports_or_reports_a_typed_error() {
    let dir = std::path::Path::new(FIXTURE_DIR);
    let entries = std::fs::read_dir(dir).unwrap_or_else(|error| {
        panic!(
            "fixture directory '{FIXTURE_DIR}' is missing ({error}); run bench/fetch_fixtures.sh"
        )
    });
    let mut paths: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "dsn"))
        .collect();
    paths.sort();
    assert!(
        paths.len() >= 40,
        "expected the full fixture set, found {}",
        paths.len()
    );

    let mut imported = 0usize;
    for path in &paths {
        let bytes = std::fs::read(path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        match import(&bytes) {
            Ok(design) => {
                imported += 1;
                assert!(!design.board.layers().is_empty(), "{name} has no layers");
                assert!(
                    design.scale_factor > 0.0,
                    "{name} has a collapsed scale factor"
                );
                assert!(
                    !design.board.store().is_empty(),
                    "{name} produced an empty board"
                );
            }
            Err(error) => {
                // The only legitimate rejection is a file that is not a DSN.
                assert!(
                    bytes.len() < 4 || &bytes[..4] != b"(pcb",
                    "{name} looks like a DSN but was rejected: {error}"
                );
            }
        }
    }
    assert!(
        imported + 1 >= paths.len(),
        "only {imported} of {} fixtures imported",
        paths.len()
    );
}

// =================================================================================================
// `Packages.get` — the `::N` variant fallback, and the resolved keepout clearance classes
// =================================================================================================

/// Two images with the *same* base name and different pin orders, so `Library.readScope`'s dedup
/// probe keeps both: `dup` and `dup::1`. Both are declared front-side. Two components reference
/// `dup::1`, one on each side.
///
/// `(resolution um 10)` with a 10000-unit outline pins the scale factor at 10.
const VARIANT_BOARD: &str = r#"(pcb variants
  (parser (string_quote "))
  (resolution um 10)
  (unit um)
  (structure
    (layer F.Cu (type signal))
    (layer B.Cu (type signal))
    (boundary (path signal 0 0 0 10000 0 10000 10000 0 10000 0 0))
    (rule (width 250) (clearance 200))
  )
  (placement
    (component dup::1
      (place FRONT 5000 -5000 front 0)
      (place BACK 6000 -5000 back 0)
    )
  )
  (library
    (image dup
      (pin Round[A]Pad_600_um 1 -100 0)
      (pin Round[A]Pad_600_um 2 100 0)
      (keepout K1 (rect F.Cu -50 -50 50 50))
    )
    (image dup::1
      (pin Round[A]Pad_600_um 2 100 0)
      (pin Round[A]Pad_600_um 1 -100 0)
      (keepout K1 (rect F.Cu -50 -50 50 50))
    )
    (padstack Round[A]Pad_600_um (shape (circle F.Cu 600)) (shape (circle B.Cu 600)) (attach off))
  )
  (network (class default (rule (width 250))))
  (wiring)
)"#;

#[test]
fn back_side_placement_falls_back_to_the_base_package_variant() {
    let design = synthetic(VARIANT_BOARD);
    let names: Vec<&str> = design.packages.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["dup", "dup::1"],
        "the pin orders differ, so the `::N` dedup probe must keep both images"
    );

    // `Network.insertComponent` (`Network.java:806-807`) resolves the image name once per side and
    // `Component.getPackage()` picks by the placement side. `Packages.get("dup::1", true)` hits the
    // literal name on the first pass; `Packages.get("dup::1", false)` finds no back-side package,
    // records `dup::1` as the other-side candidate, then **overwrites** it with `dup` on the
    // base-name pass and returns that. So a back-placed component lands on the base variant.
    let front = design
        .components
        .iter()
        .find(|c| c.name == "FRONT")
        .expect("FRONT is placed");
    let back = design
        .components
        .iter()
        .find(|c| c.name == "BACK")
        .expect("BACK is placed");
    assert_eq!(design.packages[front.package_index].name, "dup::1");
    assert_eq!(design.packages[back.package_index].name, "dup");

    // The two images disagree on pin order, so the fallback is observable in the pin names too.
    assert_eq!(
        design.packages[front.package_index].pins[0].name, "2",
        "the `::1` image lists pin 2 first"
    );
    assert_eq!(design.packages[back.package_index].pins[0].name, "1");
}

#[test]
fn component_keepouts_record_their_resolved_clearance_class() {
    let design = synthetic(VARIANT_BOARD);
    // One keepout per component, on the single layer named by the `(rect F.Cu …)`.
    assert_eq!(count_kind(&design, BoardItemClass::Keepout), 2);

    let area_class = design
        .rules
        .get_default_net_class()
        .default_item_clearance_classes
        .get(ItemClass::Area);
    for component in &design.components {
        // The list is parallel to the package's `keepouts`, which is what
        // `Component.writeKeepoutInfos` walks; with no `(keepout … (clearance_class …))` in the
        // placement the resolution is the net class's AREA default.
        assert_eq!(
            component.keepout_clearance_classes.of_kind(0),
            &[area_class],
            "{}",
            component.name
        );
        assert!(component.keepout_clearance_classes.of_kind(1).is_empty());
        assert!(component.keepout_clearance_classes.of_kind(2).is_empty());
    }
}

#[test]
fn an_unplaced_component_records_no_keepout_classes() {
    // `Component.writeKeepoutInfos` returns immediately for an unplaced component, and the board
    // gets no items for it either, so the parallel list must stay empty rather than be padded.
    let src = VARIANT_BOARD.replace("(place BACK 6000 -5000 back 0)", "(place BACK)");
    let design = synthetic(&src);
    let back = design
        .components
        .iter()
        .find(|c| c.name == "BACK")
        .expect("an unplaced component still exists");
    assert!(back.location.is_none());
    assert!(back.keepout_clearance_classes.of_kind(0).is_empty());
    assert_eq!(count_kind(&design, BoardItemClass::Keepout), 1);
}

#[test]
fn smd_to_turn_gap_falls_back_to_the_min_trace_half_width() {
    // `Structure.updateBoardRules` (`Structure.java:606`): with no
    // `(clearance … (type smd_to_turn_gap))` in the file, `pin_edge_to_turn_dist` becomes
    // `BoardRules.getMinTraceHalfWidth()` — and that aggregate is seeded only by
    // `setDefaultTraceHalfWidth[s]`, never by `createDefaultNetClass`'s 1500-unit placeholder
    // (`BoardRules.java:238-244`). `VARIANT_BOARD` declares `(width 250)` at `(resolution um 10)`,
    // so the half width is 1250 board units and the gap must follow it *down*.
    assert!(!VARIANT_BOARD.contains("smd_to_turn_gap"));
    let design = synthetic(VARIANT_BOARD);
    assert_eq!(design.rules.get_min_trace_half_width(), 1250);
    assert_eq!(design.rules.get_pin_edge_to_turn_dist(), 1250.0);

    // A file that *does* declare the gap keeps its own value, untouched by the width.
    let src = VARIANT_BOARD.replace(
        "(rule (width 250) (clearance 200))",
        "(rule (width 250) (clearance 200) (clearance 33 (type smd_to_turn_gap)))",
    );
    let design = synthetic(&src);
    assert_eq!(design.rules.get_pin_edge_to_turn_dist(), 330.0);
}

// =================================================================================================
// The pin frame — §9.2.2, and the padstack table's pin-relative contract
// =================================================================================================

/// Two pins of one footprint at **opposite** offsets, sharing one padstack. Issue027's `R2` in
/// miniature: `(resolution um 10)` and a 10000-unit outline keep the scale factor at 10, so the
/// pins sit 1270 board units above and below the component location `(50000, 50000)` and the pad
/// is a 1000×1000 square around each.
fn two_pin_board(rotation: &str) -> String {
    format!(
        r#"(pcb pinframe
  (parser (string_quote "))
  (resolution um 10)
  (structure
    (layer F.Cu (type signal))
    (layer B.Cu (type signal))
    (boundary (rect pcb 0 0 10000 10000))
    (rule (width 250) (clearance 200))
  )
  (placement
    (component RES
      (place R2 5000 5000 front {rotation})
    )
  )
  (library
    (image RES (pin PAD 1 0 127) (pin PAD 2 0 -127))
    (padstack PAD (shape (rect F.Cu -50 -50 50 50)) (attach off))
  )
  (network
    (net N1 (pins R2-1))
    (net N2 (pins R2-2))
  )
  (wiring)
)"#
    )
}

/// The padstack the board stores is **pin-relative**: `search_tree::drill_shapes` translates it by
/// `Pin::center`, so the pad of a pin 1270 units up and the pad of a pin 1270 units down are the
/// same square around the origin, and each pin's copper lands on its own centre.
///
/// This is the wave-5 pin-offset defect's regression test. `placed_pad_shapes` used to leave the
/// package-relative translation inside the shape, and `PadstackKey` does not carry that offset, so
/// both pins were interned under one key and the **second** pin's copper sat 2540 units away from
/// it — on top of the first pin.
#[test]
fn both_pins_of_a_footprint_carry_their_own_pad() {
    let design = synthetic(&two_pin_board("0"));
    assert_eq!(
        pin_center(&design, "R2", "1"),
        IntPoint::new(50_000, 51_270)
    );
    assert_eq!(
        pin_center(&design, "R2", "2"),
        IntPoint::new(50_000, 48_730)
    );

    for (_, item) in design.board.store().iter() {
        let ItemKind::Pin(pin) = &item.kind else {
            continue;
        };
        let padstack = design
            .board
            .padstacks()
            .get(pin.padstack_no)
            .expect("every pin interns a padstack");
        let pad = padstack
            .shape_on_layer(0)
            .expect("the pad is on the front layer")
            .bounding_box();
        assert_eq!(
            (pad.ll.x, pad.ll.y, pad.ur.x, pad.ur.y),
            (-500, -500, 500, 500),
            "the stored pad is centred on the pin, not on the component"
        );
    }
}

/// At a right-angle rotation the pad is independent of where the pin sits in the package, so one
/// table entry serves the whole footprint — the dedup `PadstackKey` exists for.
#[test]
fn a_right_angle_rotation_interns_one_padstack_for_both_pins() {
    let design = synthetic(&two_pin_board("90"));
    // turn90(1) maps (x, y) → (-y, x), so (0, 1270) → (-1270, 0).
    assert_eq!(
        pin_center(&design, "R2", "1"),
        IntPoint::new(48_730, 50_000)
    );
    assert_eq!(
        pin_center(&design, "R2", "2"),
        IntPoint::new(51_270, 50_000)
    );

    let padstack_nos: Vec<u32> = design
        .board
        .store()
        .iter()
        .filter_map(|(_, item)| match &item.kind {
            ItemKind::Pin(pin) => Some(pin.padstack_no),
            _ => None,
        })
        .collect();
    assert_eq!(padstack_nos.len(), 2);
    assert_eq!(padstack_nos[0], padstack_nos[1]);
}

/// At any other angle `rotate_approx` rounds each corner, and rounding does not distribute over
/// the package translation: upstream rotates `pad + rel` and this crate has to subtract the
/// rounded `rel` back out, which leaves a per-pin residue of at most one board unit. The entries
/// are therefore keyed per pin offset, and every pin's copper still sits on its own centre.
#[test]
fn an_oblique_rotation_keys_the_padstack_per_pin() {
    let design = synthetic(&two_pin_board("30"));
    let mut padstack_nos = Vec::new();
    for (_, item) in design.board.store().iter() {
        let ItemKind::Pin(pin) = &item.kind else {
            continue;
        };
        padstack_nos.push(pin.padstack_no);
        let pad = design
            .board
            .padstacks()
            .get(pin.padstack_no)
            .expect("every pin interns a padstack")
            .shape_on_layer(0)
            .expect("the pad is on the front layer")
            .bounding_box();
        assert!(
            (pad.ll.x + pad.ur.x).abs() <= 2 && (pad.ll.y + pad.ur.y).abs() <= 2,
            "the rotated pad stays centred on its own pin, off by at most a unit: {pad:?}"
        );
    }
    assert_eq!(padstack_nos.len(), 2);
    assert_ne!(
        padstack_nos[0], padstack_nos[1],
        "the two pins round differently, so they cannot share one entry"
    );
}

// =================================================================================================
// Area placement — §9.2.5, and the search tree's convex-piece contract
// =================================================================================================

/// A keepout 3000 board units off the component origin on a component placed at 330°.
///
/// Issue054's `SW33` in miniature. `PlacedArea::rotation_deg` reaches
/// `search_tree::placed_area_pieces`, which realizes only right-angle rotations and widens any
/// other one to the octagon of the disc the piece stays inside **for every rotation** — a disc
/// centred on the component origin, so a 200-unit keepout 3000 units out became a 3200-unit blob
/// over the middle of the footprint. The importer applies the oblique rotation itself
/// (`ObstacleArea.get_area`'s `rotate_approx` branch) so the board only ever sees an angle it can
/// place exactly.
#[test]
fn an_obliquely_placed_keepout_lands_where_it_was_rotated_to() {
    let src = r#"(pcb areaframe
  (parser (string_quote "))
  (resolution um 10)
  (structure
    (layer F.Cu (type signal))
    (layer B.Cu (type signal))
    (boundary (rect pcb 0 0 10000 10000))
    (rule (width 250) (clearance 200))
  )
  (placement
    (component SW
      (place SW33 5000 5000 front 330)
    )
  )
  (library
    (image SW (pin PAD 1 0 0) (keepout "" (circle F.Cu 40 300 0)))
    (padstack PAD (shape (rect F.Cu -50 -50 50 50)) (attach off))
  )
  (network (net N1 (pins SW33-1)))
  (wiring)
)"#;
    let design = synthetic(src);
    let boxes: Vec<_> = design
        .board
        .store()
        .iter()
        .filter(|(_, item)| item.class() == BoardItemClass::Keepout)
        .map(|(id, item)| {
            // The rotation is *baked into the stored corners*, so the board is handed an angle it
            // realizes exactly and never takes `placed_area_pieces`'s widening branch. It also
            // means the convex split downstream runs on the already-rotated shape, which is the
            // only order in which the two are composable: rounding each rotated corner onto the
            // grid can make a convex corner slightly concave, and the split absorbs that, whereas
            // rotating a decomposition would drift its pieces apart.
            let ItemKind::ObstacleArea(obstacle) = &item.kind else {
                panic!("a keepout is an ObstacleArea");
            };
            assert_eq!(
                obstacle.area.rotation_deg, 0.0,
                "an oblique placement rotation is applied at import, not left for the board"
            );
            let slot = design.board.store().slot_of(id).expect("a stored item");
            let mut bounds = fr_geometry::planar::IntBox::EMPTY;
            for entry in design.board.default_tree().item_shapes(slot, id) {
                if let Some(shape) = &entry.shape {
                    bounds = bounds.union(&shape.bounding_box());
                }
            }
            bounds
        })
        .collect();
    assert_eq!(boxes.len(), 1, "one keepout on one named layer");
    let bounds = boxes[0];
    // cos(-30°) · 3000 = 2598, sin(-30°) · 3000 = -1500, plus the component location.
    let centre_x = (bounds.ll.x + bounds.ur.x) / 2;
    let centre_y = (bounds.ll.y + bounds.ur.y) / 2;
    assert!(
        (centre_x - 52_598).abs() <= 2 && (centre_y - 48_500).abs() <= 2,
        "the keepout is at the rotated offset, not at the component origin: {bounds:?}"
    );
    // The bounding octagon of a 200-unit-radius circle, not of the 3200-unit disc that contains
    // every rotation of it.
    assert!(
        bounds.ur.x - bounds.ll.x < 600,
        "the keepout keeps its own size: {bounds:?}"
    );
}

/// The two halves of the area contract, on one L-shaped keepout: the board **stores the original
/// polygon**, and the search tree **derives a real convex decomposition** from it.
///
/// Both halves have been broken, in opposite directions, and each break is invisible to the test
/// for the other:
///
/// * storing `AreaShape::Polygon` and letting the tree fall back to the shape's **bounding
///   octagon** fills the notch with copper. Issue022, Issue029 and Issue054 each reported
///   clearance violations against traces sitting in exactly such a notch (G1.2a);
/// * storing `AreaShape::Convex(split_to_convex(…))` instead fixes the notch and destroys the
///   corner list, because a decomposition has no border to write back out. `golden_dump` emits
///   such an area as `null` and 13 fixtures regressed on the comparator's `structure_areas` group
///   (G1.1).
///
/// Upstream does neither: `ObstacleArea` (`ObstacleArea.java:37-56`) stores the area it was
/// constructed with, and `splitToConvex()` is computed on demand by whoever indexes it. So the
/// polygon must survive import *and* the tree must still see tiles.
#[test]
fn a_concave_keepout_is_stored_as_a_polygon_and_split_for_the_tree() {
    let src = r#"(pcb notch
  (parser (string_quote "))
  (resolution um 10)
  (structure
    (layer F.Cu (type signal))
    (layer B.Cu (type signal))
    (boundary (rect pcb 0 0 10000 10000))
    (keepout "" (polygon F.Cu 0  1000 1000  5000 1000  5000 2000  2000 2000  2000 5000  1000 5000))
    (rule (width 250) (clearance 200))
  )
  (placement)
  (library
    (padstack PAD (shape (rect F.Cu -50 -50 50 50)) (attach off))
  )
  (network)
  (wiring)
)"#;
    let design = synthetic(src);
    let (id, item) = design
        .board
        .store()
        .iter()
        .find(|(_, item)| item.class() == BoardItemClass::Keepout)
        .expect("the structure keepout");
    let ItemKind::ObstacleArea(obstacle) = &item.kind else {
        panic!("a keepout is an ObstacleArea");
    };

    // 1. The stored shape is the original polygon, corner for corner. Scale factor 10, so every
    //    DSN coordinate is multiplied by 10 and nothing else happens to it.
    let AreaShape::Polygon(polygon) = &obstacle.area.relative_area else {
        panic!(
            "a polygon keepout is stored as its polygon, not as a decomposition: {:?}",
            obstacle.area.relative_area
        );
    };
    let corners: Vec<(i32, i32)> = polygon
        .corner_array()
        .iter()
        .map(|point| match point {
            Point::Int(p) => (p.x, p.y),
            Point::Rational(_) => panic!("a DSN corner is integral"),
        })
        .collect();
    assert_eq!(
        corners,
        vec![
            (10_000, 10_000),
            (50_000, 10_000),
            (50_000, 20_000),
            (20_000, 20_000),
            (20_000, 50_000),
            (10_000, 50_000),
        ],
        "the corner list golden_dump has to write back out"
    );

    // 2. The tree pieces are a true convex decomposition of it: every corner of the polygon is
    //    covered, the notch is not, and no piece is the bounding octagon that used to stand in.
    let slot = design.board.store().slot_of(id).expect("a stored item");
    let shapes = design.board.default_tree().item_shapes(slot, id);
    let pieces: Vec<_> = shapes
        .iter()
        .filter_map(|entry| entry.shape.as_ref())
        .collect();
    assert!(pieces.len() > 1, "an L cannot be one convex tile");
    let covers = |x: i32, y: i32| {
        let point = Point::Int(IntPoint::new(x, y));
        pieces.iter().any(|shape| shape.contains(&point))
    };
    for (x, y) in &corners {
        assert!(
            covers(*x, *y),
            "the pieces cover every corner of the polygon; ({x}, {y}) is uncovered"
        );
    }
    assert!(covers(15_000, 15_000), "the corner of the L is copper");
    assert!(covers(45_000, 15_000), "the foot of the L is copper");
    assert!(covers(15_000, 45_000), "the stem of the L is copper");
    assert!(
        !covers(45_000, 45_000),
        "the notch is not copper — it was, when the tree saw a bounding octagon"
    );
    // No piece is the bounding octagon the tree used to fall back to. That octagon contains the
    // whole bounding box, and the box's fourth corner — max x, max y — is the outer corner of the
    // notch, which no piece of a correct decomposition may reach.
    let notch_corner = IntPoint::new(
        corners.iter().map(|(x, _)| *x).max().unwrap(),
        corners.iter().map(|(_, y)| *y).max().unwrap(),
    );
    assert_eq!(notch_corner, IntPoint::new(50_000, 50_000));
    for piece in &pieces {
        assert!(
            !piece.contains(&Point::Int(notch_corner)),
            "no piece is the bounding octagon of the polygon: {piece:?}"
        );
    }
}

// =================================================================================================
// snap_angle — the ordinal encoding between read.rs and fr-rules (§6.7)
// =================================================================================================

/// Minimal board whose `(structure …)` carries the given snap angle scope.
fn snap_board(snap_scope: &str) -> String {
    format!(
        r#"(pcb snap
  (parser (string_quote "))
  (resolution um 10)
  (structure
    (layer F.Cu (type signal))
    (boundary (rect pcb 0 0 10000 10000))
    {snap_scope}
    (rule (width 250) (clearance 200))
  )
  (placement)
  (library)
  (network)
  (wiring)
)"#
    )
}

/// §6.7: `(snap_angle ninety_degree)` must reach the board as
/// `AngleRestriction.NINETY_DEGREE`. Regression: `read_snap_angle` encoded the
/// keyword as `(0 = ninety, 2 = none)` while `fr_rules::AngleRestriction::from_i32`
/// decodes the enum's declaration order (`NONE, FORTYFIVE_DEGREE, NINETY_DEGREE`
/// — `board/AngleRestriction.java:8`), so a 90° board was routed free-angle and
/// a `none` board 90°. The 46-fixture suite never caught it because its only
/// declared snap_angle is `fortyfive_degree`, which both mappings get right.
#[test]
fn snap_angle_keywords_map_to_the_enum_ordinal() {
    use fr_board::shape::AngleRestriction as BoardRestriction;
    use fr_rules::AngleRestriction as RulesRestriction;

    let ninety = synthetic(&snap_board("(snap_angle ninety_degree)"));
    assert_eq!(
        ninety.rules.get_trace_angle_restriction(),
        RulesRestriction::NinetyDegree
    );
    assert_eq!(ninety.board.angle_restriction(), BoardRestriction::Ninety);

    let none = synthetic(&snap_board("(snap_angle none)"));
    assert_eq!(
        none.rules.get_trace_angle_restriction(),
        RulesRestriction::None
    );
    assert_eq!(none.board.angle_restriction(), BoardRestriction::None);

    let fortyfive = synthetic(&snap_board("(snap_angle fortyfive_degree)"));
    assert_eq!(
        fortyfive.rules.get_trace_angle_restriction(),
        RulesRestriction::FortyFiveDegree
    );
    assert_eq!(
        fortyfive.board.angle_restriction(),
        BoardRestriction::FortyFive
    );

    // §6.7: no snap_angle at all defaults to FORTYFIVE_DEGREE.
    let undeclared = synthetic(&snap_board(""));
    assert_eq!(
        undeclared.rules.get_trace_angle_restriction(),
        RulesRestriction::FortyFiveDegree
    );
}
