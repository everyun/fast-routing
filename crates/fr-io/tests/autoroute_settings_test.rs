//! `(structure (autoroute_settings …))` read *and* applied.
//!
//! The unit tests inside `specctra::autoroute_settings` cover the merge in isolation; these
//! drive it from DSN text through the real reader, because the two halves have a shared failure
//! mode that neither half can see on its own: a field that parses into the AST and is then never
//! looked at again is indistinguishable, from inside the parser, from a field that works.
//!
//! The scope was parsed but never applied until this wave. `Issue103-Board-Routed.dsn` says
//! `(postroute off)`, upstream honours it and finishes the board in 1.2 s, and we ran the
//! optimizer over it until the 285 s budget ran out.

use fr_io::specctra::{apply_autoroute_settings, import, ImportedDesign};
use fr_rules::router_settings::{BoardAspectRatio, RouterSettings};

/// A two-signal-layer board with the `autoroute_settings` scope spliced in wherever the caller
/// puts `{settings}` — before the keepout, or after it, which is what the guard bug turns on.
fn board_with(settings: &str, keepout_first: bool) -> String {
    let keepout = "    (keepout \"\" (rect F.Cu 8000 8000 9000 9000))\n";
    let (first, second) = if keepout_first {
        (keepout, settings)
    } else {
        (settings, keepout)
    };
    format!(
        r#"(pcb autoroute_settings_probe
  (parser (string_quote "))
  (resolution um 10)
  (unit um)
  (structure
    (layer F.Cu (type signal))
    (layer B.Cu (type signal))
    (boundary (path signal 0 0 0 10000 0 10000 10000 0 10000 0 0))
    (rule (width 250) (clearance 200))
{first}{second}  )
  (placement
    (component pad2
      (place P1 1000 1000 front 0)
      (place P2 6000 1000 front 0)
    )
  )
  (library
    (image pad2 (pin round_pad 1 0 0))
    (padstack round_pad (shape (circle F.Cu 400)) (shape (circle B.Cu 400)) (attach off))
  )
  (network
    (net SIG (pins P1-1 P2-1))
    (class default SIG (rule (width 250) (clearance 200)))
  )
)"#
    )
}

/// The whole scope, as `Issue103-Board-Routed.dsn` and `Issue187-processor.Z80.dsn` write it —
/// every keyword the reader has a branch for, including the two it must discard.
const FULL_SCOPE: &str = "    (autoroute_settings
      (fanout on)
      (autoroute off)
      (postroute off)
      (vias off)
      (via_costs 50)
      (plane_via_costs 5)
      (start_ripup_costs 100)
      (start_pass_no 25)
      (layer_rule F.Cu
        (active on)
        (preferred_direction horizontal)
        (preferred_direction_trace_costs 1.0)
        (against_preferred_direction_trace_costs 2.1)
      )
      (layer_rule B.Cu
        (active off)
        (preferred_direction vertical)
        (preferred_direction_trace_costs 1.0)
        (against_preferred_direction_trace_costs 1.9)
      )
    )\n";

fn design(source: &str) -> ImportedDesign {
    import(source.as_bytes()).expect("the probe board imports")
}

/// What `fr_core::pipeline::settings_for` does, in the order it does it.
fn settings_for(design: &ImportedDesign) -> RouterSettings {
    let mut settings = RouterSettings::default();
    settings.set_layer_count(design.rules.layer_structure.len());
    if let Some(dsn) = &design.autoroute_settings {
        apply_autoroute_settings(&mut settings, dsn);
    }
    settings.apply_board_specific_optimizations(
        BoardAspectRatio::from_bounding_box(
            f64::from(design.bounds.width()),
            f64::from(design.bounds.height()),
        ),
        &design.rules.layer_structure,
    );
    settings
}

/// Every field with an upstream sink, end to end. One test rather than eight because the point
/// is that they all come off the *same* scope: a reader that mis-counts one sub-scope's tokens
/// corrupts the ones after it, and eight separate one-field boards would never show that.
#[test]
fn every_applied_field_reaches_the_router_settings() {
    let design = design(&board_with(FULL_SCOPE, false));
    let dsn = design
        .autoroute_settings
        .as_ref()
        .expect("the scope precedes the keepout, so the guard lets it through");

    // Parsed…
    assert_eq!(dsn.run_router, Some(false));
    assert_eq!(dsn.run_optimizer, Some(false));
    assert_eq!(dsn.vias_allowed, Some(false));
    assert_eq!(dsn.via_costs, Some(50));
    assert_eq!(dsn.plane_via_costs, Some(5));
    assert_eq!(dsn.start_ripup_costs, Some(100));
    assert_eq!(dsn.layer_settings.len(), 2);
    assert_eq!(dsn.layer_settings[0].layer_name, "F.Cu");
    assert_eq!(dsn.layer_settings[0].layer_no, 0);
    assert_eq!(dsn.layer_settings[1].layer_name, "B.Cu");
    assert_eq!(dsn.layer_settings[1].layer_no, 1);

    // …and applied.
    let settings = settings_for(&design);
    assert!(!settings.run_router(), "(autoroute off)");
    assert!(!settings.run_optimizer(), "(postroute off)");
    assert!(!settings.vias_allowed, "(vias off)");
    assert_eq!(settings.scoring.via_costs(), 50);
    assert_eq!(settings.scoring.plane_via_costs(), 5);
    assert_eq!(settings.scoring.start_ripup_costs(), 100);
    assert!(settings.layer_active(0), "F.Cu (active on)");
    assert!(!settings.layer_active(1), "B.Cu (active off)");
    assert!(settings.preferred_direction_is_horizontal(0));
    assert!(!settings.preferred_direction_is_horizontal(1));
}

/// `(start_pass_no …)` has no branch in upstream's reader at all, and the per-layer trace costs
/// are wiped by the `applyBoardSpecificOptimizations` that follows the merge. Both are parsed
/// here — a golden dump should still be able to show what the file asked for — and neither may
/// change a number the router reads.
#[test]
fn the_two_fields_upstream_throws_away_change_nothing() {
    let bare = design(&board_with("", false));
    let design = design(&board_with(FULL_SCOPE, false));
    let dsn = design.autoroute_settings.as_ref().expect("parsed");
    assert_eq!(
        dsn.layer_settings[0].against_preferred_direction_trace_costs,
        Some(2.1)
    );
    assert_eq!(
        dsn.layer_settings[1].against_preferred_direction_trace_costs,
        Some(1.9)
    );

    let with_scope = settings_for(&design);
    let without_scope = settings_for(&bare);

    assert_eq!(
        with_scope.scoring.preferred_direction_trace_cost,
        without_scope.scoring.preferred_direction_trace_cost,
    );
    assert_eq!(
        with_scope.scoring.undesired_direction_trace_cost,
        without_scope.scoring.undesired_direction_trace_cost,
        "2.1/1.9 never reach the maze; the board's aspect ratio decides these"
    );
    // `start_pass_no 25` is not a pass count, a ripup seed, or anything else.
    assert_eq!(with_scope.max_passes, without_scope.max_passes);
    assert_eq!(
        with_scope.scoring.start_ripup_costs(),
        100,
        "…and consuming it did not eat the scope that follows it"
    );
}

/// The guard bug, from the applying end: a keepout ahead of the scope means the settings are
/// never parsed, so *none* of them are applied — including the `(postroute off)` that would
/// otherwise switch the optimizer off. Dropped upstream, dropped here.
#[test]
fn a_scope_the_guard_dropped_applies_nothing() {
    let design = design(&board_with(FULL_SCOPE, true));
    assert!(
        design.autoroute_settings.is_none(),
        "the keepout materialized the layer structure first"
    );

    let settings = settings_for(&design);
    assert!(settings.run_router(), "the default, not the file's `off`");
    assert!(
        settings.run_optimizer(),
        "the default, not the file's `off`"
    );
    assert!(settings.vias_allowed);
    assert!(
        settings.layer_active(1),
        "B.Cu's `(active off)` is gone too"
    );
}

/// `AutorouteSettings.read_layer_rule` returns `null` when the layer name does not resolve, and
/// `read_scope` propagates that as its own `null` — so one bad `layer_rule` costs the file every
/// other setting in the scope, not just that rule.
#[test]
fn a_layer_rule_naming_an_unknown_layer_drops_the_whole_scope() {
    let scope = "    (autoroute_settings
      (postroute off)
      (via_costs 50)
      (layer_rule In1.Cu (active off))
      (vias off)
    )\n";
    let design = design(&board_with(scope, false));
    assert!(design.autoroute_settings.is_none());

    let settings = settings_for(&design);
    assert!(
        settings.run_optimizer(),
        "even the `postroute off` ahead of it"
    );
    assert!(settings.vias_allowed);
    assert_eq!(
        settings.scoring.via_costs(),
        50,
        "the shipped default, unchanged"
    );

    // The rest of the file still reads: the abort leaves the remaining tokens to the structure
    // loop, which skips them one scope at a time.
    assert_eq!(design.board.layers().len(), 2);
    assert!(design
        .warnings
        .iter()
        .any(|w| w.contains("read_layer_rule: layer not found")));
}

/// A scope that mentions neither `autoroute` nor `postroute` still writes both, because
/// upstream's two locals are seeded `true` and assigned unconditionally at the end of the read.
/// It is the only path by which a DSN file can turn the optimizer *on*.
#[test]
fn a_scope_without_the_two_switches_still_writes_their_defaults() {
    let design = design(&board_with(
        "    (autoroute_settings (via_costs 7))\n",
        false,
    ));
    let dsn = design.autoroute_settings.as_ref().expect("parsed");
    assert_eq!(dsn.run_router, None);
    assert_eq!(dsn.run_optimizer, None);

    let mut settings = RouterSettings::default();
    settings.set_layer_count(2);
    settings.enabled = Some(false);
    settings.optimizer.enabled = Some(false);
    apply_autoroute_settings(&mut settings, dsn);
    assert!(settings.run_router());
    assert!(settings.run_optimizer());
}
