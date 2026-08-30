//! `(structure (autoroute_settings …))` → [`RouterSettings`].
//!
//! [`read`](crate::specctra::read) parses the scope into a [`DsnAutorouteSettings`]; this is the
//! other half, the part that makes the file's opinions reach the router. Upstream's parser writes
//! its fields straight onto a **fresh** `RouterSettings`
//! (`io/specctra/parser/AutorouteSettings.read_scope`) and `Structure.read_scope:979-987` merges
//! that object into the job's settings with `RouterSettings.applyNewValuesFrom` →
//! `ReflectionUtil.copyFields`. The merge is field-by-field over the *public* fields, copying
//! every non-null one — so the scope's effect is the union of "what `read_scope` set" and "what
//! `copyFields` is able to carry".
//!
//! # Two of the parsed fields never reach the router, and must not reach ours either
//!
//! **`(start_pass_no n)`** — `Keyword.START_PASS_NO` exists, and `read_scope` has no branch for
//! it. The scope goes to `ScopeKeyword.skip_scope` and the number is discarded before it is ever
//! a setting. `read` consumes it the same way.
//!
//! **`(layer_rule … (preferred_direction_trace_costs …) (against_preferred_direction_trace_costs
//! …))`** — these land in `scoring.preferredDirectionTraceCost[]` /
//! `scoring.undesiredDirectionTraceCost[]`, which are `double[]`, i.e. an array of a *primitive*
//! component type, and `copyFields` refuses to copy one of those onto a target that already has a
//! non-empty array. Even when the target's arrays are still null — a job whose settings are fresh
//! — the values are copied and then immediately overwritten: the flag that would protect them,
//! `boardSpecificTraceCostsApplied`, is `private`, so `copyFields` skips it, the merged settings
//! carry `false`, and the `applyBoardSpecificOptimizations` that every load path runs next
//! (`HeadlessBoardManager.applyRouterSettingsForLoadedBoard:628`,
//! `RoutingJobScheduler:108`) recomputes both arrays from the board's aspect ratio.
//!
//! Measured against the release jar rather than argued from the source: with the Issue187 scope
//! replayed through `RouterSettings.applyNewValuesFrom`, a fresh target comes out
//! `againstCost0 = 2.5` **and** `areBoardSpecificTraceCostsApplied() == false`, and a target whose
//! arrays were pre-sized comes out `againstCost0 = 1.0`. Either way the DSN's number is gone by
//! the time the maze reads it.
//!
//! So [`apply`] applies neither, and [`crate::specctra::ast::DsnLayerSetting`] keeps both parsed
//! so that a golden dump can still show what the file asked for.
//!
//! # Everything else does reach it
//!
//! | scope | upstream sink | reader |
//! |---|---|---|
//! | `(autoroute on\|off)` | `enabled` → `getRunRouter()` | gates the batch loop (`RoutingJobSchedulerActionThread:96`) |
//! | `(postroute on\|off)` | `optimizer.enabled` → `getRunOptimizer()` | gates the optimization stage (`:179`) |
//! | `(vias on\|off)` | `vias_allowed` | `AutorouteControl:156` |
//! | `(via_costs n)` | `scoring.viaCosts`, clamped `>= 1` | `BatchAutorouter:1272` |
//! | `(plane_via_costs n)` | `scoring.planeViaCosts`, clamped `>= 1` | `BatchAutorouter:1270`, for a net that touches a plane |
//! | `(start_ripup_costs n)` | `scoring.startRipupCosts`, clamped `>= 1` | pass *n* routes at `start_ripup_costs × n` |
//! | `(layer_rule L (active on\|off))` | `layers[L].routable` | `AutorouteControl:166` |
//! | `(layer_rule L (preferred_direction …))` | `layers[L].preferredDirectionHorizontal` | the per-layer expansion cost pair |
//!
//! # `(autoroute …)` and `(postroute …)` are written even when the file omits them
//!
//! `read_scope`'s two locals are seeded `true` and assigned **unconditionally** at `:63-64`, so a
//! scope that mentions neither still merges `enabled = true` and `optimizer.enabled = true` into
//! the job. That matters in one direction only — `getRunOptimizer()` reads `optimizer.enabled`
//! fail-closed, so a file that carries an `autoroute_settings` scope at all is a file that turns
//! the optimizer *on* if the job had it off. [`apply`] reproduces it.
//!
//! # Where it is called from
//!
//! Between sizing the settings to the board and
//! [`RouterSettings::apply_board_specific_optimizations`], which is where upstream's merge lands
//! relative to its own call: `layers[i].routable` and `layers[i].preferredDirectionHorizontal`
//! are set here and survive, because that function only fills in the entries that are still
//! `None`. A non-signal layer that the file marks `(active on)` is forced back off there, exactly
//! as upstream forces it.

use fr_rules::router_settings::RouterSettings;

use crate::specctra::ast::DsnAutorouteSettings;

/// Merges a parsed `(autoroute_settings …)` scope into `settings`.
///
/// Call before [`RouterSettings::apply_board_specific_optimizations`]; see the module docs for
/// why the two per-layer trace-cost fields are deliberately not applied.
pub fn apply(settings: &mut RouterSettings, dsn: &DsnAutorouteSettings) {
    // `AutorouteSettings.java:62-64`: both locals default to `true` and are always assigned.
    settings.enabled = Some(dsn.run_router.unwrap_or(true));
    settings.optimizer.enabled = Some(dsn.run_optimizer.unwrap_or(true));

    if let Some(vias_allowed) = dsn.vias_allowed {
        settings.vias_allowed = vias_allowed;
    }
    if let Some(via_costs) = dsn.via_costs {
        settings.scoring.set_via_costs(via_costs);
    }
    if let Some(plane_via_costs) = dsn.plane_via_costs {
        settings.scoring.set_plane_via_costs(plane_via_costs);
    }
    if let Some(start_ripup_costs) = dsn.start_ripup_costs {
        settings.scoring.set_start_ripup_costs(start_ripup_costs);
    }

    for layer in &dsn.layer_settings {
        // The index was resolved and range-checked by the reader; an unresolvable one dropped
        // the whole scope there.
        if let Some(active) = layer.active {
            settings.set_layer_active(layer.layer_no, active);
        }
        if let Some(is_horizontal) = layer.preferred_direction_is_horizontal {
            settings.set_preferred_direction_is_horizontal(layer.layer_no, is_horizontal);
        }
        // `preferred_direction_trace_costs` / `against_preferred_direction_trace_costs`:
        // deliberately dropped. See the module docs.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::specctra::ast::DsnLayerSetting;
    use fr_rules::router_settings::BoardAspectRatio;
    use fr_rules::{Layer, LayerStructure};

    fn two_signal_layers() -> LayerStructure {
        LayerStructure::new(vec![
            Layer {
                name: "F.Cu".to_string(),
                is_signal: true,
            },
            Layer {
                name: "B.Cu".to_string(),
                is_signal: true,
            },
        ])
    }

    fn sized_settings() -> RouterSettings {
        let mut settings = RouterSettings::default();
        settings.set_layer_count(2);
        settings
    }

    #[test]
    fn postroute_off_switches_the_optimizer_off() {
        let mut settings = sized_settings();
        assert!(settings.run_optimizer(), "the shipped default is on");
        apply(
            &mut settings,
            &DsnAutorouteSettings {
                run_optimizer: Some(false),
                ..DsnAutorouteSettings::default()
            },
        );
        assert!(!settings.run_optimizer());
    }

    /// `read_scope` seeds `with_postroute = true` and assigns it unconditionally, so a scope that
    /// never mentions `postroute` still turns the optimizer on.
    #[test]
    fn an_absent_postroute_still_writes_the_default_on() {
        let mut settings = sized_settings();
        settings.optimizer.enabled = Some(false);
        settings.enabled = Some(false);
        apply(&mut settings, &DsnAutorouteSettings::default());
        assert!(settings.run_optimizer());
        assert!(settings.run_router());
    }

    #[test]
    fn autoroute_off_switches_the_router_off() {
        let mut settings = sized_settings();
        apply(
            &mut settings,
            &DsnAutorouteSettings {
                run_router: Some(false),
                ..DsnAutorouteSettings::default()
            },
        );
        assert!(!settings.run_router());
    }

    #[test]
    fn vias_off_is_applied() {
        let mut settings = sized_settings();
        assert!(settings.vias_allowed);
        apply(
            &mut settings,
            &DsnAutorouteSettings {
                vias_allowed: Some(false),
                ..DsnAutorouteSettings::default()
            },
        );
        assert!(!settings.vias_allowed);
    }

    /// All three cost scopes go through the upstream setters, which clamp to `>= 1`.
    #[test]
    fn the_three_cost_scopes_are_applied_and_clamped() {
        let mut settings = sized_settings();
        apply(
            &mut settings,
            &DsnAutorouteSettings {
                via_costs: Some(50),
                plane_via_costs: Some(5),
                start_ripup_costs: Some(100),
                ..DsnAutorouteSettings::default()
            },
        );
        assert_eq!(settings.scoring.via_costs(), 50);
        assert_eq!(settings.scoring.plane_via_costs(), 5);
        assert_eq!(settings.scoring.start_ripup_costs(), 100);

        apply(
            &mut settings,
            &DsnAutorouteSettings {
                via_costs: Some(0),
                plane_via_costs: Some(-7),
                start_ripup_costs: Some(0),
                ..DsnAutorouteSettings::default()
            },
        );
        assert_eq!(settings.scoring.via_costs(), 1);
        assert_eq!(settings.scoring.plane_via_costs(), 1);
        assert_eq!(settings.scoring.start_ripup_costs(), 1);
    }

    /// `(active off)` survives `apply_board_specific_optimizations`, which only fills in `None`.
    #[test]
    fn a_layer_rule_can_switch_a_layer_off() {
        let mut settings = sized_settings();
        apply(
            &mut settings,
            &DsnAutorouteSettings {
                layer_settings: vec![DsnLayerSetting {
                    layer_name: "B.Cu".to_string(),
                    layer_no: 1,
                    active: Some(false),
                    ..DsnLayerSetting::default()
                }],
                ..DsnAutorouteSettings::default()
            },
        );
        assert!(settings.layer_active(0));
        assert!(!settings.layer_active(1));

        settings.apply_board_specific_optimizations(
            BoardAspectRatio::from_bounding_box(100.0, 50.0),
            &two_signal_layers(),
        );
        assert!(settings.layer_active(0), "untouched layers stay routable");
        assert!(!settings.layer_active(1), "and the file's `off` survives");
    }

    /// `(preferred_direction …)` overrides the direction the board geometry would have picked,
    /// and — as upstream — does *not* change which of the two aspect-ratio surcharges is added,
    /// because that one is driven by the geometry's own alternation.
    #[test]
    fn a_layer_rule_can_override_the_preferred_direction() {
        let layers = two_signal_layers();
        let aspect = BoardAspectRatio::from_bounding_box(100.0, 50.0);

        let mut geometry_only = sized_settings();
        geometry_only.apply_board_specific_optimizations(aspect, &layers);
        let geometry_costs = geometry_only.scoring.undesired_direction_trace_cost.clone();

        let mut settings = sized_settings();
        apply(
            &mut settings,
            &DsnAutorouteSettings {
                layer_settings: vec![
                    DsnLayerSetting {
                        layer_name: "F.Cu".to_string(),
                        layer_no: 0,
                        preferred_direction_is_horizontal: Some(true),
                        ..DsnLayerSetting::default()
                    },
                    DsnLayerSetting {
                        layer_name: "B.Cu".to_string(),
                        layer_no: 1,
                        preferred_direction_is_horizontal: Some(false),
                        ..DsnLayerSetting::default()
                    },
                ],
                ..DsnAutorouteSettings::default()
            },
        );
        settings.apply_board_specific_optimizations(aspect, &layers);

        assert!(settings.preferred_direction_is_horizontal(0));
        assert!(!settings.preferred_direction_is_horizontal(1));
        assert_eq!(
            settings.scoring.undesired_direction_trace_cost, geometry_costs,
            "the surcharge follows the geometry's alternation, not the file's directions"
        );
    }

    /// The upstream drop, pinned: a `layer_rule`'s two trace-cost numbers are parsed and then
    /// never reach the router, because `applyBoardSpecificOptimizations` recomputes both arrays.
    #[test]
    fn layer_rule_trace_costs_are_dropped_the_way_upstream_drops_them() {
        let layers = two_signal_layers();
        let aspect = BoardAspectRatio::from_bounding_box(100.0, 50.0);

        let mut geometry_only = sized_settings();
        geometry_only.apply_board_specific_optimizations(aspect, &layers);

        let mut settings = sized_settings();
        apply(
            &mut settings,
            &DsnAutorouteSettings {
                layer_settings: vec![DsnLayerSetting {
                    layer_name: "F.Cu".to_string(),
                    layer_no: 0,
                    preferred_direction_trace_costs: Some(9.0),
                    against_preferred_direction_trace_costs: Some(9.0),
                    ..DsnLayerSetting::default()
                }],
                ..DsnAutorouteSettings::default()
            },
        );
        settings.apply_board_specific_optimizations(aspect, &layers);

        assert_eq!(
            settings.scoring.preferred_direction_trace_cost,
            geometry_only.scoring.preferred_direction_trace_cost,
        );
        assert_eq!(
            settings.scoring.undesired_direction_trace_cost,
            geometry_only.scoring.undesired_direction_trace_cost,
        );
    }
}
