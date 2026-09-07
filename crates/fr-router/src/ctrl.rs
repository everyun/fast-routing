//! `AutorouteControl` — the per-net derived cost and geometry parameters.
//!
//! Transcribed from `autoroute/AutorouteControl.java` (release 2.3.0
//! @2d4de019). Every deviation from the spec body is a conformance-appendix
//! correction and is called out at the line that implements it.

use fr_board::board::Board;
use fr_board::clearance::ClearanceSource;
use fr_board::item::{ItemClass, ItemId};
use fr_rules::board_rules::BoardRules;
use fr_rules::router_settings::RouterSettings;
use fr_rules::via_rule::ViaRule;

/// Horizontal and vertical trace-cost factors on one layer
/// (`AutorouteControl.ExpansionCostFactor`, `:298-313`).
///
/// The same type `fr-rules` already derives from the board aspect ratio, so
/// the control carries the settings' values through unchanged.
pub use fr_rules::router_settings::ExpansionCostFactor;

/// One entry of `via_info_arr`: the layer span of a padstack in the net's via
/// rule, plus its attach-SMD flag (`AutorouteControl.ViaMask`, `:327-338`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViaMask {
    /// First layer the padstack spans.
    pub from_layer: i32,
    /// Last layer the padstack spans.
    pub to_layer: i32,
    /// Whether this padstack may land on an SMD pad.
    pub attach_smd_allowed: bool,
}

/// Upstream's `TRACE_WIDTH_TOLERANCE` (`AutorouteEngine.java:33`), added to
/// every half width in the door-sectioning and small-door tests.
pub const TRACE_WIDTH_TOLERANCE: i32 = 2;

/// `MazeSearchAlgo.ALREADY_RIPPED_COSTS` (`:43`).
pub const ALREADY_RIPPED_COSTS: i32 = 1;

/// `calc_fanout_via_ripup_cost_factor`'s `FANOUT_COST_CONST` (`:155`).
pub const FANOUT_COST_CONST: f64 = 20_000.0;

/// `MazeSearchAlgo`'s `MAX_RIPUP_COSTS` (`:1454`), verified as `21474836` in
/// the shipped bytecode.
pub const MAX_RIPUP_COSTS: i32 = i32::MAX / 100;

/// The pull-tight accuracy the control hands to the inserter (`:182`).
pub const PULL_TIGHT_ACCURACY: i32 = 500;

/// Shove recursion depths (`:183-185`).
pub const MAX_SHOVE_TRACE_RECURSION_DEPTH: i32 = 20;
/// See [`MAX_SHOVE_TRACE_RECURSION_DEPTH`].
pub const MAX_SHOVE_VIA_RECURSION_DEPTH: i32 = 5;
/// See [`MAX_SHOVE_TRACE_RECURSION_DEPTH`].
pub const MAX_SPRING_OVER_RECURSION_DEPTH: i32 = 5;

/// The factor a pure-SMD net's via cost is multiplied by
/// (`AutorouteControl.java:274`; conformance appendix §3(b)).
pub const PURE_SMD_VIA_COST_FACTOR: f64 = 0.1;

/// `min_cheap_via_cost = 0.8 × min_normal_via_cost` (`:277`).
pub const CHEAP_VIA_COST_FACTOR: f64 = 0.8;

/// The per-net cost and geometry parameters of one maze search.
///
/// Built by [`AutorouteControl::for_net`], which is
/// `AutorouteControl(board, net, settings)` — the private constructor followed
/// by `init_net` followed by `rebuild_via_info`.
#[derive(Debug, Clone)]
pub struct AutorouteControl {
    /// Number of board layers.
    pub layer_count: usize,
    /// The net being routed.
    pub net_no: i32,
    /// Per-layer `(horizontal, vertical)` cost factors; the edge cost of the
    /// maze is `weighted_distance` under these (`MazeSearchAlgo.java:735-738`).
    pub trace_costs: Vec<ExpansionCostFactor>,
    /// Per-layer flat bend penalty, clamped to `[0.0, 9.9]`, default `0.0`
    /// (conformance appendix §4(a)).
    pub bend_costs: Vec<f64>,
    /// Which layers may carry a trace. Power planes are forced inactive here,
    /// matching `AutorouteControl.java:166-173`.
    pub layer_active: Vec<bool>,
    /// Net-class trace half width per layer.
    pub trace_half_width: Vec<i32>,
    /// [`trace_half_width`](Self::trace_half_width) plus the clearance
    /// compensation of the trace clearance class — the radius every query
    /// against the compensated tree uses (`:220`).
    pub compensated_trace_half_width: Vec<i32>,
    /// Per-layer via radius: the largest padstack radius of the net's via rule
    /// on that layer, floored by the trace half width (`:252, :266`).
    pub via_radius_arr: Vec<f64>,
    /// `add_via_costs[from][to]`. All zero in the release and never reassigned
    /// (`:186-190`); kept because it is the intended blind/buried-via hook.
    pub add_via_costs: Vec<Vec<i32>>,
    /// The clearance class traces are routed in.
    pub trace_clearance_class_no: i32,
    /// Clearance class of the *first* via of the rule (`:229-233`).
    pub via_clearance_class: i32,
    /// The net's via rule, cloned so the control is self-contained.
    pub via_rule: ViaRule,
    /// One [`ViaMask`] per padstack in the via rule.
    pub via_info_arr: Vec<ViaMask>,
    /// Lower bound for the first layer of a via.
    pub via_lower_bound: i32,
    /// Upper bound for the last layer of a via.
    pub via_upper_bound: i32,
    /// Largest via radius over all layers, floored by the trace half widths.
    pub max_via_radius: f64,
    /// `via_costs × max(max_via_radius, 1)`, times `0.1` for a pure-SMD net.
    pub min_normal_via_cost: f64,
    /// `0.8 × min_normal_via_cost`; the batch ordering's cheap variant.
    pub min_cheap_via_cost: f64,
    /// Whether layer changes are allowed at all.
    pub vias_allowed: bool,
    /// Whether a via may land on an SMD pad. A pure-SMD net forces this true
    /// (`:257-263`).
    pub attach_smd_allowed: bool,
    /// Whether the search may plan ripups.
    pub ripup_allowed: bool,
    /// The ripup toll seed. The batch loop sets `start_ripup_costs ×
    /// ripup_pass_no`; the ctor default of 1000 is always overwritten, and the
    /// effective pass-1 value is **100** (conformance appendix D5).
    pub ripup_costs: i32,
    /// Always `1` in the release, which makes the randomization branch of
    /// `check_ripup` unreachable (conformance appendix D4).
    pub ripup_pass_no: i32,
    /// The `start_ripup_costs` the fanout-protection gate compares against
    /// (`MazeSearchAlgo.java:1386-1387`).
    pub start_ripup_costs: i32,
    /// Fanout mode: the search stops at the first via.
    pub is_fanout: bool,
    /// Fanout escape gating: the start pin's centre, when known.
    pub fanout_start_pin_center: Option<fr_geometry::planar::IntPoint>,
    /// Fanout escape gating: the start pin's layer, `-1` when unset.
    pub fanout_start_pin_layer: i32,
    /// Maximum fanout escape length in board units; `None` disables the gate.
    pub fanout_max_escape_length: Option<f64>,
    /// Minimum fanout escape length in board units; `None` disables the gate.
    pub fanout_min_escape_length: Option<f64>,
    /// Normally true; false during the fanout pre-pass, which is what keeps the
    /// fanout ripup protection alive.
    pub remove_unconnected_vias: bool,
    /// Whether a failed insertion keeps its ripup instead of rolling back —
    /// the batch router's late-game policy (REPORT-latefail), `false` for the
    /// fanout path and by default.
    pub fail_rips: bool,
    /// Automatic neckdown at pins.
    pub with_neckdown: bool,
    /// Shove recursion depth for traces. `0` disables shoving; M2a leaves the
    /// shove hooks stubbed regardless (see [`crate::maze::search`]).
    pub max_shove_trace_recursion_depth: i32,
    /// Shove recursion depth for vias.
    pub max_shove_via_recursion_depth: i32,
    /// Spring-over recursion depth.
    pub max_spring_over_recursion_depth: i32,
    /// Pull-tight accuracy handed on to the inserter.
    pub pull_tight_accuracy: i32,
}

impl AutorouteControl {
    /// Builds the control for `net_no`, exactly as
    /// `AutorouteControl(RoutingBoard, int, RouterSettings)` does: the shared
    /// constructor, then `init_net`, then `rebuild_via_info`.
    ///
    /// `settings` must already have been through
    /// [`RouterSettings::apply_board_specific_optimizations`] — that is where
    /// the per-layer preferred direction and the against-preferred-direction
    /// cost factors that feed [`Self::trace_costs`] come from, and running the
    /// maze without it silently routes at cost 1.0 in both directions on every
    /// layer.
    pub fn for_net(
        board: &Board,
        rules: &BoardRules,
        settings: &RouterSettings,
        net_no: i32,
    ) -> AutorouteControl {
        let via_costs = settings.scoring.via_costs();
        Self::for_net_with_costs(board, rules, settings, net_no, via_costs)
    }

    /// [`for_net`](Self::for_net) with an explicit via cost — the fanout pass's
    /// second constructor (`AutorouteControl.java:142-145`).
    pub fn for_net_with_costs(
        board: &Board,
        rules: &BoardRules,
        settings: &RouterSettings,
        net_no: i32,
        via_costs: i32,
    ) -> AutorouteControl {
        let layer_count = board.layers().len();
        let trace_cost_arr = settings.trace_cost_arr();
        // A settings object that has not been sized for the board yields fewer
        // entries than the board has layers; upstream would have thrown, we
        // fall back to the neutral 1.0/1.0 pair so the engine is still usable
        // from a bare `RouterSettings::default()` in tests.
        let trace_costs: Vec<ExpansionCostFactor> = (0..layer_count)
            .map(|layer| {
                trace_cost_arr
                    .get(layer)
                    .copied()
                    .unwrap_or(ExpansionCostFactor {
                        horizontal: 1.0,
                        vertical: 1.0,
                    })
            })
            .collect();

        // `AutorouteControl.java:160-173`: bend costs come straight from the
        // settings (already clamped there), and a non-signal layer that the
        // settings marked active is forced inactive.
        let bend_costs: Vec<f64> = (0..layer_count).map(|i| settings.bend_cost(i)).collect();
        let layer_active: Vec<bool> = (0..layer_count)
            .map(|i| {
                let active_setting = settings.layer_active(i);
                if !board.layers().is_signal(i as i32) && active_setting {
                    false
                } else {
                    active_setting
                }
            })
            .collect();

        let mut ctrl = AutorouteControl {
            layer_count,
            net_no,
            trace_costs,
            bend_costs,
            layer_active,
            trace_half_width: vec![0; layer_count],
            compensated_trace_half_width: vec![0; layer_count],
            via_radius_arr: vec![0.0; layer_count],
            add_via_costs: vec![vec![0; layer_count]; layer_count],
            trace_clearance_class_no: 1,
            via_clearance_class: 1,
            via_rule: ViaRule::empty(),
            via_info_arr: Vec::new(),
            via_lower_bound: 0,
            via_upper_bound: layer_count as i32,
            max_via_radius: 0.0,
            min_normal_via_cost: 0.0,
            min_cheap_via_cost: 0.0,
            vias_allowed: settings.vias_allowed,
            attach_smd_allowed: false,
            ripup_allowed: false,
            // `:197`: the ctor default. Every real call site overwrites it with
            // `start_ripup_costs × ripup_pass_no` (D5).
            ripup_costs: 1000,
            ripup_pass_no: 1,
            start_ripup_costs: settings.scoring.start_ripup_costs(),
            is_fanout: false,
            fanout_start_pin_center: None,
            fanout_start_pin_layer: -1,
            fanout_max_escape_length: None,
            fanout_min_escape_length: None,
            remove_unconnected_vias: true,
            fail_rips: false,
            with_neckdown: settings.automatic_neckdown,
            max_shove_trace_recursion_depth: MAX_SHOVE_TRACE_RECURSION_DEPTH,
            max_shove_via_recursion_depth: MAX_SHOVE_VIA_RECURSION_DEPTH,
            max_spring_over_recursion_depth: MAX_SPRING_OVER_RECURSION_DEPTH,
            pull_tight_accuracy: PULL_TIGHT_ACCURACY,
        };
        ctrl.init_net(board, rules, net_no, via_costs);
        ctrl
    }

    /// `AutorouteControl.init_net` (`:201-226`).
    fn init_net(&mut self, board: &Board, rules: &BoardRules, net_no: i32, via_costs: i32) {
        let net_class = rules
            .nets
            .get(net_no)
            .map(|net| net.net_class_index())
            .and_then(|index| rules.net_classes.get(index));

        match net_class {
            Some(class) => {
                self.trace_clearance_class_no = class.get_trace_clearance_class() as i32;
                self.via_rule = class
                    .get_via_rule()
                    .cloned()
                    .or_else(|| {
                        let default_rule = rules.get_default_via_rule()?;
                        let via_cl = class
                            .default_item_clearance_classes
                            .get(fr_rules::ItemClass::Via)
                            as usize;
                        let mut adjusted = ViaRule::new(default_rule.name.clone());
                        for i in 0..default_rule.via_count() {
                            if let Some(v) = default_rule.get_via(i) {
                                let mut v = v.clone();
                                v.set_clearance_class(via_cl);
                                adjusted.append_via(v);
                            }
                        }
                        Some(adjusted)
                    })
                    .unwrap_or_else(ViaRule::empty);
            }
            None => {
                // `:210-212`: no net → clearance class 1 and the board's first
                // via rule.
                self.trace_clearance_class_no = 1;
                self.via_rule = rules
                    .get_default_via_rule()
                    .cloned()
                    .unwrap_or_else(ViaRule::empty);
            }
        }

        for layer in 0..self.layer_count {
            let half_width = if net_no > 0 {
                rules.get_trace_half_width(net_no, layer)
            } else {
                rules.get_trace_half_width(1, layer)
            };
            self.trace_half_width[layer] = half_width;
            self.compensated_trace_half_width[layer] = half_width
                + board
                    .clearances()
                    .compensation_value(self.trace_clearance_class_no, layer as i32);
            if let Some(class) = net_class {
                if !class.is_active_routing_layer(layer) {
                    self.layer_active[layer] = false;
                }
            }
        }
        self.rebuild_via_info(board, via_costs, net_no);
    }

    /// `AutorouteControl.rebuild_via_info` (`:228-278`).
    ///
    /// Re-invoked by the fanout path after its via-rule fallback merge
    /// (`RoutingBoard.java:1036`), which is why it is public.
    pub fn rebuild_via_info(&mut self, board: &Board, via_costs: i32, net_no: i32) {
        self.via_clearance_class = self
            .via_rule
            .get_via(0)
            .map(|via| via.clearance_class() as i32)
            .unwrap_or(1);
        self.via_info_arr.clear();
        self.attach_smd_allowed = false;
        self.via_radius_arr.iter_mut().for_each(|r| *r = 0.0);

        for index in 0..self.via_rule.via_count() {
            let Some(via) = self.via_rule.get_via(index) else {
                continue;
            };
            if via.attach_smd_allowed() {
                self.attach_smd_allowed = true;
            }
            let padstack = via.padstack();
            let from_layer = padstack.from_layer();
            let to_layer = padstack.to_layer();
            for layer in from_layer..=to_layer.min(self.layer_count.saturating_sub(1)) {
                let radius = padstack
                    .get_shape(layer)
                    .map(|shape| 0.5 * shape.max_width())
                    .unwrap_or(0.0);
                self.via_radius_arr[layer] = self.via_radius_arr[layer].max(radius);
            }
            self.via_info_arr.push(ViaMask {
                from_layer: from_layer as i32,
                to_layer: to_layer as i32,
                attach_smd_allowed: via.attach_smd_allowed(),
            });
        }

        // Conformance appendix §3(b): the two pure-SMD asymmetries. The attach
        // relaxation is gated on `!attach_smd_allowed && layer_count > 1`; the
        // ×0.1 is not, and it applies *after* the `max(…, 1)` floor.
        let pure_smd_net = is_pure_smd_net(board, net_no);
        if !self.attach_smd_allowed && self.layer_count > 1 && pure_smd_net {
            self.attach_smd_allowed = true;
        }

        self.max_via_radius = 0.0;
        for layer in 0..self.layer_count {
            self.via_radius_arr[layer] =
                self.via_radius_arr[layer].max(self.trace_half_width[layer] as f64);
            self.max_via_radius = self.max_via_radius.max(self.via_radius_arr[layer]);
        }
        let mut via_cost_factor = self.max_via_radius.max(1.0);
        if pure_smd_net {
            via_cost_factor *= PURE_SMD_VIA_COST_FACTOR;
        }
        self.min_normal_via_cost = via_costs as f64 * via_cost_factor;
        self.min_cheap_via_cost = CHEAP_VIA_COST_FACTOR * self.min_normal_via_cost;
    }

    /// The half width every query on `layer` is inflated by, saturating at the
    /// last layer so callers can pass a drill layer without bounds-checking.
    #[inline]
    pub fn compensated_half_width(&self, layer: i32) -> f64 {
        let index = (layer.max(0) as usize).min(self.compensated_trace_half_width.len() - 1);
        self.compensated_trace_half_width[index] as f64
    }

    /// The `(h, v)` cost pair on `layer`.
    #[inline]
    pub fn trace_cost(&self, layer: i32) -> ExpansionCostFactor {
        let index = (layer.max(0) as usize).min(self.trace_costs.len() - 1);
        self.trace_costs[index]
    }

    /// Whether `layer` may carry a trace.
    #[inline]
    pub fn layer_is_active(&self, layer: i32) -> bool {
        self.layer_active
            .get(layer.max(0) as usize)
            .copied()
            .unwrap_or(false)
    }
}

/// `AutorouteControl.isPureSmdNet` (`:280-293`): the net has connectable items
/// and every one of them is a single-layer pin.
fn is_pure_smd_net(board: &Board, net_no: i32) -> bool {
    let items: &[ItemId] = board.store().net_items(net_no);
    if items.is_empty() {
        return false;
    }
    items.iter().all(|id| {
        board
            .store()
            .get(*id)
            .map(|item| item.class() == ItemClass::Pin && item.first_layer() == item.last_layer())
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{empty_board, TestFixture};

    #[test]
    fn min_normal_via_cost_is_via_costs_times_the_radius_floor() {
        // An empty board: no connectable items, so `isPureSmdNet` is false and
        // the ×0.1 factor does not apply (`AutorouteControl.java:280-293`).
        let fixture = TestFixture::empty_two_layer();
        let ctrl = fixture.control();
        // `min_normal_via_cost = via_costs × max(max_via_radius, 1)`
        // (`AutorouteControl.java:269-276`).
        let expected = fixture.settings.scoring.via_costs() as f64 * ctrl.max_via_radius.max(1.0);
        assert!((ctrl.min_normal_via_cost - expected).abs() < 1e-9);
        assert!(
            (ctrl.min_cheap_via_cost - CHEAP_VIA_COST_FACTOR * ctrl.min_normal_via_cost).abs()
                < 1e-9
        );
    }

    #[test]
    fn add_via_costs_are_all_zero() {
        let fixture = TestFixture::two_layer_with_pads();
        let ctrl = fixture.control();
        for row in &ctrl.add_via_costs {
            assert!(row.iter().all(|cost| *cost == 0));
        }
    }

    #[test]
    fn power_planes_are_forced_inactive() {
        use fr_board::board::{Layer, LayerStructure};
        let layers = LayerStructure::new(vec![
            Layer::signal("front"),
            Layer::plane("gnd"),
            Layer::signal("back"),
        ]);
        let (board, rules, mut settings) = empty_board(layers);
        settings.set_layer_active(1, true);
        let ctrl = AutorouteControl::for_net(&board, &rules, &settings, 1);
        assert!(
            !ctrl.layer_active[1],
            "a plane layer must never be routable"
        );
    }

    #[test]
    fn compensated_half_width_adds_the_class_compensation() {
        let fixture = TestFixture::two_layer_with_pads();
        let ctrl = fixture.control();
        for layer in 0..ctrl.layer_count {
            let compensation = fixture
                .board
                .clearances()
                .compensation_value(ctrl.trace_clearance_class_no, layer as i32);
            assert_eq!(
                ctrl.compensated_trace_half_width[layer],
                ctrl.trace_half_width[layer] + compensation
            );
        }
    }

    #[test]
    fn pure_smd_net_gets_the_tenth_via_cost_and_forced_attach() {
        let fixture = TestFixture::pure_smd_two_layer();
        let ctrl = fixture.control();
        assert!(ctrl.attach_smd_allowed, "pure-SMD relaxes the attach gate");
        let without_factor =
            fixture.settings.scoring.via_costs() as f64 * ctrl.max_via_radius.max(1.0);
        assert!(
            (ctrl.min_normal_via_cost - without_factor * PURE_SMD_VIA_COST_FACTOR).abs() < 1e-9,
            "pure-SMD via cost must be ×0.1: {} vs {}",
            ctrl.min_normal_via_cost,
            without_factor
        );
    }
}
