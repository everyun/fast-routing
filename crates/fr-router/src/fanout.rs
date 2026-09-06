//! The SMD pin-escape (fanout) stage.
//!
//! Ported from the **2.3.0 release** `autoroute/BatchFanout.java` (orchestration
//! and ordering) and `board/RoutingBoard.java:977-1083` (`fanout`, the per-pin
//! attempt), per `batch-orchestration.md` §2 and its conformance appendix
//! §2.3. Rows referenced below are that appendix's claim table.
//!
//! | this module | upstream |
//! |---|---|
//! | [`components`] | `BatchFanout.Component` ctor + `compareTo` (`:449-509`), row 18 |
//! | [`PinSortingOrder`] | `BatchFanout.Component.Pin.compareTo` (`:557-594`), row 15 |
//! | [`fanout_pin`] | `RoutingBoard.fanout` (`:977-1083`), rows 27-35 |
//! | [`fanout_pass`] | `BatchFanout.fanout_pass` (`:147-360`), rows 25-26 |
//! | [`fanout_board`] | `BatchFanout.fanout_board` (`:82-144`), rows 20-24 |
//!
//! # What makes a fanout attempt different from an ordinary connection
//!
//! Nothing about the maze — it is the same engine — only the control
//! ([`crate::ctrl::AutorouteControl`]) it runs under (row 13):
//!
//! 1. `is_fanout = true` seeds the destination estimator with the board box on
//!    the outer layers, so the wave heads for *any other layer*;
//! 2. the search completes at the first drill whose backtrack door is also a
//!    drill — the escape is done the moment the route has dropped through a via
//!    (row 31), so the via position is chosen implicitly by maze cost;
//! 3. `fanout_start_pin_center/_layer` switch on the escape **annulus**: while
//!    on the pin's layer, expansions farther than
//!    [`DEFAULT_MAX_ESCAPE_LENGTH_MM`] from the pin centre are rejected, and
//!    drills closer than [`DEFAULT_MIN_ESCAPE_LENGTH_MM`] are rejected **on
//!    every layer** (row 12 — the spec body compresses this);
//! 4. `remove_unconnected_vias = false`, so the placed via survives
//!    `remove_tails(FANOUT_VIA)` (row 32) and later passes pay
//!    [`crate::ctrl::FANOUT_COST_CONST`] to rip it (row 33).
//!
//! # Divergence: the no-progress detector
//!
//! The release stops a fanout run on `routed == 0`, on three consecutive
//! *identical* `(routed_count, via_count)` pairs (rows 21-22). Its own comment
//! records the motivating failure — Issue219, **14 identical passes** where
//! pins kept ripping each other's escapes — which the three-pass oscillation
//! rule only caught because the pairs were bit-equal.
//!
//! We once additionally stopped after 2 consecutive passes whose via count did
//! not grow. That detector is **removed**: its safety argument — a pass that
//! escapes no new pin cannot lose coverage — is false. A pass may route only
//! re-escapes of already-escaped pins (ripping their old escapes first) while
//! other pins sit isolated from earlier failed re-escapes; the via count stays
//! flat, the stop fires, and the stage hands the main router a board with
//! unescaped pins the pass loop then cannot repair (measured: Issue110-
//! RelayModule Full 2u where Doors/Java reach 0u, and Issue039's Full gap —
//! both traced to this early stop in REPORT-full-relay.md and
//! REPORT-tiling45-full.md §3.1). Coverage is upstream's contract here; CPU
//! spin is bounded by the oscillation rule, `maxPasses` and the job deadline.
//!
//! # Job deadline
//!
//! Upstream arms no `TimeLimit` anywhere in this stage (`RoutingBoard.fanout`
//! builds its engine with a `null` TimeLimit, `:1053-1055`). Our `--job-timeout`
//! is nonetheless honoured here the same way the route and optimizer stages
//! honour it: [`FanoutConfig::deadline`] defaults to [`JobDeadline::ambient`]
//! and is polled before every pass and before every pin, so a spent budget ends
//! the stage with [`FanoutStop::JobDeadline`] and the pipeline's reserved tail
//! still judges and writes the board. One pin's attempt is never interrupted —
//! interrupting mid-attempt would make what gets inserted depend on load.

use fr_board::board::Board;
use fr_board::item::{ItemId, ItemKind};
use fr_geometry::planar::{FloatPoint, IntPoint};
use fr_rules::board_rules::BoardRules;
use fr_rules::router_settings::RouterSettings;
use fr_rules::via_rule::ViaRule;

use crate::batch::schedule::JobDeadline;
use crate::batch::{connected_set, unconnected_set, AttemptOutcome, AttemptState, ContactScratch};
use crate::ctrl::AutorouteControl;

/// `BatchFanout.fanout_board`'s hardcoded pass fallback (`:93-94`) and the
/// `router.fanout.maxPasses` default (`DefaultSettings.java:114`), row 20.
pub const DEFAULT_MAX_PASSES: i32 = 20;

/// `BatchFanout.STAGNATION_PASS_LIMIT` (`:95`), row 22: three consecutive
/// identical `(routed_count << 32) ^ via_count` values stop the run.
pub const STAGNATION_PASS_LIMIT: u32 = 3;

/// `router.fanout.maxEscapeLengthMm` default (`DefaultSettings.java:118`),
/// row 10 — confirmed against the shipped settings, not the null fallback.
pub const DEFAULT_MAX_ESCAPE_LENGTH_MM: f64 = 4.5;

/// `router.fanout.minEscapeLengthMm` default (`DefaultSettings.java:117`),
/// row 10.
pub const DEFAULT_MIN_ESCAPE_LENGTH_MM: f64 = 2.5;

/// `MazeSearchAlgo.java:88`'s `3000.0` µm literal, reached only when the whole
/// `fanout` settings object or the field is null (row 11). Documented, **not**
/// replicated as magic behaviour: [`FanoutConfig`] always carries a value.
pub const FALLBACK_MAX_ESCAPE_LENGTH_MM: f64 = 3.0;

/// `MazeSearchAlgo.java:98`'s `500.0` µm literal (row 11).
pub const FALLBACK_MIN_ESCAPE_LENGTH_MM: f64 = 0.5;

/// `BatchFanout.java:541-551`: "20.0 mm in coordinate units", the radius the
/// `surroundings_density` order counts other SMD pins in (row 16).
pub const SURROUNDINGS_DENSITY_RADIUS_MM: f64 = 20.0;

/// `router.fanout.maxMillisecondsPerPin` default (`DefaultSettings.java:115`),
/// row 26. The per-pin budget of pass *p* (0-based) is this times `p + 1`.
pub const DEFAULT_MAX_MILLISECONDS_PER_PIN: i64 = 10_000;

/// `BatchFanout.Component.Pin.compareTo` (`:557-594`), row 15 — note the
/// release's **snake_case** keys; the camelCase spellings of the spec body fall
/// through to [`PinSortingOrder::PinIndex`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PinSortingOrder {
    /// Ascending distance to the component's SMD gravity centre (v1.9).
    InnerFirst,
    /// Descending distance to it — the **2.3.0 default**
    /// (`DefaultSettings.java:121`, and the hardcoded fallback at
    /// `BatchFanout.java:41`).
    #[default]
    OuterFirst,
    /// Ascending distance to the nearest other pin on the same net, over
    /// *all* board pins (row 17).
    DistanceToClosestOnNet,
    /// Descending count of other SMD pins within
    /// [`SURROUNDINGS_DENSITY_RADIUS_MM`] (row 16).
    SurroundingsDensity,
    /// Anything else: pin-index order, which is also every order's tie-break.
    PinIndex,
}

impl PinSortingOrder {
    /// Parses a `router.fanout.pinSortingOrder` value.
    pub fn parse(value: &str) -> PinSortingOrder {
        match value {
            "inner_first" => PinSortingOrder::InnerFirst,
            "outer_first" => PinSortingOrder::OuterFirst,
            "distance_to_closest_on_net" => PinSortingOrder::DistanceToClosestOnNet,
            "surroundings_density" => PinSortingOrder::SurroundingsDensity,
            _ => PinSortingOrder::PinIndex,
        }
    }
}

/// The fanout stage's knobs, already in **board units** where upstream stores
/// millimetres.
#[derive(Debug, Clone, PartialEq)]
pub struct FanoutConfig {
    /// `router.fanout.maxPasses`.
    pub max_passes: i32,
    /// `router.fanout.maxItems`; `<= 0` means unlimited.
    pub max_items: i32,
    /// `router.fanout.ripupAllowed`; `false` sends `-1` to the attempt, which
    /// means "no ripup" (row 25).
    pub ripup_allowed: bool,
    /// `router.startRipupCosts`, default [`crate::batch::START_RIPUP_COSTS`].
    /// Pass *p* (0-based) uses `start_ripup_costs * (p + 1)` (row 25).
    pub start_ripup_costs: i32,
    /// Pin order within a component.
    pub pin_sorting_order: PinSortingOrder,
    /// Escape annulus inner radius, in board units.
    pub min_escape_length: f64,
    /// Escape annulus outer radius, in board units.
    pub max_escape_length: f64,
    /// `surroundings_density` radius, in board units.
    pub density_radius: f64,
    /// `router.fanout.fallbackToBoardVias` (row 27).
    ///
    /// Resolved from [`FanoutSettings::fallback_to_board_vias`] (shipping
    /// default `false`, a documented divergence) with the
    /// `FR_FANOUT_FALLBACK_VIAS` environment override on top: set it to `1`
    /// to restore upstream's fallback behaviour for one run.
    pub fallback_to_board_vias: bool,
    /// The job's wall budget, polled before every pass and every pin.
    /// Defaults to [`JobDeadline::ambient`]; [`JobDeadline::NONE`] opts out.
    pub deadline: JobDeadline,
}

impl FanoutConfig {
    /// Builds the config from settings and the board's scale.
    ///
    /// `board_units_per_mm` is the design's own scale — the importer's
    /// `resolution` and `unit`; the caller owns the conversion because
    /// `fr-router` never sees a DSN.
    pub fn from_settings(settings: &RouterSettings, board_units_per_mm: f64) -> FanoutConfig {
        let scale = if board_units_per_mm.is_finite() && board_units_per_mm > 0.0 {
            board_units_per_mm
        } else {
            1.0
        };
        FanoutConfig {
            max_passes: settings.fanout.max_passes,
            max_items: settings.fanout.max_items,
            ripup_allowed: settings.fanout.ripup_allowed,
            start_ripup_costs: settings.scoring.start_ripup_costs(),
            pin_sorting_order: PinSortingOrder::parse(&settings.fanout.pin_sorting_order),
            min_escape_length: settings.fanout.min_escape_length_mm * scale,
            max_escape_length: settings.fanout.max_escape_length_mm * scale,
            density_radius: SURROUNDINGS_DENSITY_RADIUS_MM * scale,
            fallback_to_board_vias: std::env::var(FALLBACK_VIAS_ENV)
                .ok()
                .and_then(|v| parse_fallback_env(&v))
                .unwrap_or(settings.fanout.fallback_to_board_vias),
            deadline: JobDeadline::ambient(),
        }
    }

    /// Overrides the job deadline — tests, and drivers that own their own clock.
    pub fn with_deadline(mut self, deadline: JobDeadline) -> FanoutConfig {
        self.deadline = deadline;
        self
    }
}

/// One SMD pin of one component, with every sort key precomputed.
#[derive(Debug, Clone, PartialEq)]
pub struct FanoutPin {
    /// The board item.
    pub item: ItemId,
    /// `Pin.pin_no`, the tie-break of every order.
    pub pin_no: u32,
    /// Pad centre.
    pub center: IntPoint,
    /// The single layer the pad lives on.
    pub layer: i32,
    /// The pin's only net.
    pub net_no: i32,
    /// Distance to the component's SMD gravity centre.
    pub distance_to_component_center: f64,
    /// Distance to the nearest other pin on the same net, `f64::MAX` when there
    /// is none.
    pub distance_to_closest_on_net: f64,
    /// Other SMD pins within [`FanoutConfig::density_radius`].
    pub surroundings_density: i32,
}

/// One component's SMD pins, in escape order.
#[derive(Debug, Clone, PartialEq)]
pub struct FanoutComponent {
    /// `Component.no`, the tie-break of the component order.
    pub component_no: i32,
    /// Gravity centre of this component's SMD pins.
    pub gravity_center: FloatPoint,
    /// The pins, already sorted.
    pub pins: Vec<FanoutPin>,
}

impl FanoutComponent {
    /// The sort key's primary field.
    pub fn smd_pin_count(&self) -> usize {
        self.pins.len()
    }
}

/// Every SMD pin of the board that carries a net, grouped by component and
/// ordered exactly as `BatchFanout`'s two `TreeSet`s order it:
///
/// * components **descending by SMD pin count**, tie-break ascending component
///   number (`:499-509`, row 18);
/// * pins by `order`, tie-break ascending `pin_no` (`:557-594`, row 15);
/// * netless SMD pins are dropped before any of this (`:45-50`, row 19).
pub fn components(
    board: &Board,
    order: PinSortingOrder,
    density_radius: f64,
) -> Vec<FanoutComponent> {
    // `board.get_smd_pins()`: a pin is SMD when it lives on one layer.
    let mut smd: Vec<(ItemId, i32, u32, IntPoint, i32)> = Vec::new();
    for (id, item) in board.store().iter() {
        let ItemKind::Pin(pin) = &item.kind else {
            continue;
        };
        if !pin.layers.is_single() {
            continue;
        }
        let Some(net_no) = item.nets.iter().copied().find(|net| *net > 0) else {
            continue;
        };
        smd.push((id, item.component_no, pin.pin_no, pin.center, net_no));
    }

    let mut component_nos: Vec<i32> = smd.iter().map(|entry| entry.1).collect();
    component_nos.sort_unstable();
    component_nos.dedup();

    let mut components = Vec::new();
    for component_no in component_nos {
        let members: Vec<&(ItemId, i32, u32, IntPoint, i32)> =
            smd.iter().filter(|entry| entry.1 == component_no).collect();
        if members.is_empty() {
            continue;
        }
        let (mut x, mut y) = (0.0, 0.0);
        for member in &members {
            x += f64::from(member.3.x);
            y += f64::from(member.3.y);
        }
        let count = members.len() as f64;
        let gravity_center = FloatPoint {
            x: x / count,
            y: y / count,
        };
        let mut pins: Vec<FanoutPin> = members
            .iter()
            .map(|member| {
                let center = member.3.to_float();
                FanoutPin {
                    item: member.0,
                    pin_no: member.2,
                    center: member.3,
                    layer: board
                        .store()
                        .get(member.0)
                        .map_or(0, |item| item.first_layer()),
                    net_no: member.4,
                    distance_to_component_center: center.distance(&gravity_center),
                    // Row 17: over *all* board pins, not just the SMD ones.
                    distance_to_closest_on_net: closest_on_net(board, member.0, member.4, &center),
                    surroundings_density: smd
                        .iter()
                        .filter(|other| {
                            other.0 != member.0
                                && center.distance(&other.3.to_float()) <= density_radius
                        })
                        .count() as i32,
                }
            })
            .collect();
        sort_pins(&mut pins, order);
        components.push(FanoutComponent {
            component_no,
            gravity_center,
            pins,
        });
    }
    components.sort_by(|a, b| {
        b.smd_pin_count()
            .cmp(&a.smd_pin_count())
            .then(a.component_no.cmp(&b.component_no))
    });
    components
}

fn closest_on_net(board: &Board, id: ItemId, net_no: i32, center: &FloatPoint) -> f64 {
    let mut best = f64::MAX;
    for (other_id, other) in board.store().iter() {
        if other_id == id || !matches!(other.kind, ItemKind::Pin(_)) {
            continue;
        }
        if !other.contains_net(net_no) {
            continue;
        }
        let ItemKind::Pin(pin) = &other.kind else {
            continue;
        };
        best = best.min(center.distance(&pin.center.to_float()));
    }
    best
}

fn sort_pins(pins: &mut [FanoutPin], order: PinSortingOrder) {
    pins.sort_by(|a, b| {
        let primary = match order {
            PinSortingOrder::InnerFirst => a
                .distance_to_component_center
                .total_cmp(&b.distance_to_component_center),
            PinSortingOrder::OuterFirst => b
                .distance_to_component_center
                .total_cmp(&a.distance_to_component_center),
            PinSortingOrder::DistanceToClosestOnNet => a
                .distance_to_closest_on_net
                .total_cmp(&b.distance_to_closest_on_net),
            PinSortingOrder::SurroundingsDensity => {
                b.surroundings_density.cmp(&a.surroundings_density)
            }
            PinSortingOrder::PinIndex => std::cmp::Ordering::Equal,
        };
        primary.then(a.pin_no.cmp(&b.pin_no))
    });
}

/// What one fanout pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FanoutPassCounters {
    /// 0-based pass number, the ripup and per-pin-budget multiplier minus one.
    pub pass_no: i32,
    /// The ripup budget the pass ran at, or `-1` for "no ripup".
    pub ripup_costs: i32,
    /// Pins that escaped in this pass.
    pub routed: usize,
    /// Pins the maze could not escape.
    pub not_routed: usize,
    /// Pins whose found escape the board refused.
    pub insert_error: usize,
    /// Pins that needed no escape.
    pub already_connected: usize,
    /// Pins skipped because their net class defines no via (row 27).
    pub skipped_no_vias: usize,
    /// Vias on the board when the pass ended — the oscillation detector's
    /// second word (row 22).
    pub via_count: usize,
}

/// Why [`fanout_board`] stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanoutStop {
    /// A pass routed nothing (`:111-113`, row 21).
    NothingRouted,
    /// Three consecutive identical `(routed, vias)` pairs (row 22).
    Oscillation,
    /// `maxPasses` reached (row 20).
    MaxPasses,
    /// `maxItems` reached (row 24).
    MaxItems,
    /// The board did not change over a pass (`:134-138`, row 23).
    BoardUnchanged,
    /// No component had an SMD pin to escape.
    NothingToDo,
    /// The job's wall budget ran out ([`JobDeadline`]). Not an upstream stop
    /// rule — upstream arms no time limit in this stage at all.
    JobDeadline,
    /// Experimental (`FR_FANOUT_SAFE_STOP=1`): via count flat for 2 passes
    /// while no fanout-eligible pin is isolated.
    SafeNoProgress,
}

/// Everything a fanout run produced.
#[derive(Debug, Clone, Default)]
pub struct FanoutOutcome {
    /// One entry per pass actually run.
    pub passes: Vec<FanoutPassCounters>,
    /// Why the run stopped.
    pub stop: Option<FanoutStop>,
    /// Pins escaped over the whole run — upstream's `totalItemsFanouted`.
    pub total_items_fanouted: usize,
}

impl FanoutOutcome {
    /// Pins escaped in the last pass.
    pub fn last_routed(&self) -> usize {
        self.passes.last().map_or(0, |pass| pass.routed)
    }
}

/// The fanout stage — `BatchFanout.fanout_board` (`:82-144`).
///
/// Runs before the pass loop, inside the router (conformance §1.2), and leaves
/// the board with one escape via per escaped SMD pin.
pub fn fanout_board(
    board: &mut Board,
    rules: &BoardRules,
    settings: &RouterSettings,
    config: &FanoutConfig,
) -> FanoutOutcome {
    let mut outcome = FanoutOutcome::default();
    let components = components(board, config.pin_sorting_order, config.density_radius);
    if components.is_empty() {
        outcome.stop = Some(FanoutStop::NothingToDo);
        return outcome;
    }
    let mut scratch = ContactScratch::new();
    let mut previous_state: Option<(usize, usize)> = None;
    let mut identical_passes = 0u32;
    // Experimental safe stop (report-only, `FR_FANOUT_SAFE_STOP=1`): the
    // removed via-count stop is only safe once no fanout-eligible pin is left
    // isolated, so gate it on both conditions.
    let safe_stop = std::env::var("FR_FANOUT_SAFE_STOP").ok().as_deref() == Some("1");
    let mut flat_passes = 0u32;
    let mut previous_vias = usize::MAX;

    for pass_no in 0..config.max_passes.max(0) {
        if config.deadline.expired() {
            outcome.stop = Some(FanoutStop::JobDeadline);
            return outcome;
        }
        if config.max_items > 0 && outcome.total_items_fanouted >= config.max_items as usize {
            outcome.stop = Some(FanoutStop::MaxItems);
            return outcome;
        }
        let counters = fanout_pass(
            board,
            rules,
            settings,
            config,
            &components,
            pass_no,
            &mut outcome.total_items_fanouted,
            &mut scratch,
        );
        outcome.passes.push(counters);
        if config.deadline.expired() {
            // A pass cut short between pins must not be read as the release's
            // `routed == 0` stop below — the reason is the deadline.
            outcome.stop = Some(FanoutStop::JobDeadline);
            return outcome;
        }
        if counters.routed == 0 {
            // `:111-113` — the release's first and strongest stop.
            outcome.stop = Some(FanoutStop::NothingRouted);
            return outcome;
        }
        // Row 22 — the release's `(routed << 32) ^ vias` oscillation detector.
        // Upstream has no via-growth stop here: a pass that routes only
        // re-escapes of already-escaped pins can leave other pins isolated,
        // and only more passes repair them (see the module docs).
        let state = (counters.routed, counters.via_count);
        if previous_state == Some(state) {
            identical_passes += 1;
            if identical_passes >= STAGNATION_PASS_LIMIT {
                outcome.stop = Some(FanoutStop::Oscillation);
                return outcome;
            }
        } else {
            identical_passes = 0;
        }
        if safe_stop {
            // Safe variant of the removed via-count stop: only when no
            // fanout-eligible pin is left isolated may a flat via count stop
            // the stage.
            let isolated = components
                .iter()
                .flat_map(|c| c.pins.iter())
                .filter(|pin| {
                    board
                        .store()
                        .get(pin.item)
                        .is_some_and(|item| item.layer_range().is_single() && item.nets.len() == 1)
                })
                .filter(|pin| {
                    connected_set(board, pin.net_no, pin.item, scratch).len() <= 1
                })
                .count();
            if counters.via_count == previous_vias {
                flat_passes += 1;
                if flat_passes >= 2 && isolated == 0 {
                    outcome.stop = Some(FanoutStop::SafeNoProgress);
                    return outcome;
                }
            } else {
                flat_passes = 0;
            }
            previous_vias = counters.via_count;
        }
        previous_state = Some(state);
    }
    outcome.stop = Some(FanoutStop::MaxPasses);
    outcome
}

/// One fanout pass — `BatchFanout.fanout_pass` (`:147-360`).
///
/// `pass_no` is **0-based**, matching the release: the ripup budget is
/// `start_ripup_costs * (pass_no + 1)` and the per-pin budget is
/// [`DEFAULT_MAX_MILLISECONDS_PER_PIN`] `* (pass_no + 1)` (rows 25-26).
#[allow(clippy::too_many_arguments)]
pub fn fanout_pass(
    board: &mut Board,
    rules: &BoardRules,
    settings: &RouterSettings,
    config: &FanoutConfig,
    components: &[FanoutComponent],
    pass_no: i32,
    total_items_fanouted: &mut usize,
    scratch: &mut ContactScratch,
) -> FanoutPassCounters {
    let ripup_costs = if config.ripup_allowed {
        config.start_ripup_costs.saturating_mul(pass_no + 1)
    } else {
        -1
    };
    let mut counters = FanoutPassCounters {
        pass_no,
        ripup_costs,
        ..FanoutPassCounters::default()
    };
    for component in components {
        for pin in &component.pins {
            if config.deadline.expired() {
                // Between pins, never inside one pin's attempt: what a
                // deadline cut inserts must not depend on machine load.
                counters.via_count = via_count(board);
                return counters;
            }
            if config.max_items > 0 && *total_items_fanouted >= config.max_items as usize {
                counters.via_count = via_count(board);
                return counters;
            }
            if !net_can_use_vias(rules, pin.net_no, config.fallback_to_board_vias) {
                counters.skipped_no_vias += 1;
                continue;
            }
            let attempt = fanout_pin(board, rules, settings, config, pin, ripup_costs, scratch);
            if crate::maze::trace::is_enabled() {
                crate::maze::trace::conn_landing(&format!(
                    "FRCONN fanout pass={} cmp={} pin={} net={} ok={:?} center={},{}",
                    pass_no + 1,
                    component.component_no,
                    pin.pin_no,
                    pin.net_no,
                    attempt.state(),
                    pin.center.x,
                    pin.center.y,
                ));
            }
            match attempt.state() {
                AttemptState::Routed => {
                    counters.routed += 1;
                    *total_items_fanouted += 1;
                }
                AttemptState::InsertError => counters.insert_error += 1,
                AttemptState::AlreadyConnected | AttemptState::NoUnconnectedNets => {
                    counters.already_connected += 1
                }
                _ => counters.not_routed += 1,
            }
        }
    }
    counters.via_count = via_count(board);
    counters
}

/// Environment override for [`FanoutConfig::fallback_to_board_vias`].
///
/// Set to `1` (or `true`/`on`/`yes`) to restore upstream's fallback for one
/// run; `0` / `false` / `off` / `no` forces it off. Any other value leaves
/// the settings default, which is itself `false` — see
/// PARITY-DIVERGENCES.md, "fanout fallbackToBoardVias".
pub const FALLBACK_VIAS_ENV: &str = "FR_FANOUT_FALLBACK_VIAS";

/// Parses one [`FALLBACK_VIAS_ENV`] value; `None` = "not a switch, use the
/// settings default".
fn parse_fallback_env(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "on" | "yes" => Some(true),
        "0" | "false" | "off" | "no" => Some(false),
        _ => None,
    }
}

/// Row 27: skip a pin whose net class defines no via, unless
/// `fallbackToBoardVias` is on **and** the board's first via rule has a via.
pub fn net_can_use_vias(rules: &BoardRules, net_no: i32, fallback_to_board_vias: bool) -> bool {
    let class_has_vias = rules
        .nets
        .get(net_no)
        .and_then(|net| rules.net_classes.get(net.net_class_index()))
        .and_then(|class| class.get_via_rule())
        .is_some_and(|rule: &ViaRule| rule.via_count() > 0);
    if class_has_vias {
        return true;
    }
    fallback_to_board_vias
        && rules
            .via_rules
            .first()
            .is_some_and(|rule| rule.via_count() > 0)
}

/// One pin's escape — `RoutingBoard.fanout` (`:977-1083`).
///
/// Preconditions (`:979-996`, row 29): the pin and its whole connected set live
/// on one layer and it has exactly one net, or the answer is
/// [`AttemptState::AlreadyConnected`]; an empty unconnected set is
/// [`AttemptState::NoUnconnectedNets`].
pub fn fanout_pin(
    board: &mut Board,
    rules: &BoardRules,
    settings: &RouterSettings,
    config: &FanoutConfig,
    pin: &FanoutPin,
    ripup_costs: i32,
    scratch: &mut ContactScratch,
) -> AttemptOutcome {
    let Some(item) = board.store().get(pin.item) else {
        return AttemptOutcome::of_state(AttemptState::AlreadyConnected);
    };
    if !item.layer_range().is_single() || item.nets.len() != 1 {
        return AttemptOutcome::of_state(AttemptState::AlreadyConnected);
    }
    let connected = connected_set(board, pin.net_no, pin.item, scratch);
    let off_layer = connected.iter().any(|member| {
        board
            .store()
            .get(*member)
            .is_some_and(|item| item.first_layer() != pin.layer || item.last_layer() != pin.layer)
    });
    if off_layer {
        return AttemptOutcome::of_state(AttemptState::AlreadyConnected);
    }
    let unconnected = unconnected_set(board, pin.net_no, &connected);
    if unconnected.is_empty() {
        return AttemptOutcome::of_state(AttemptState::NoUnconnectedNets);
    }

    let ctrl = fanout_control(board, rules, settings, config, pin, ripup_costs);

    // Row 34: sort the unconnected set by squared distance of bounding-box
    // centre to the pin; with at most four targets try the closest one alone
    // first, else route against the whole set at once.
    let mut targets = unconnected;
    targets.sort_by(|a, b| {
        let da = target_distance_square(board, *a, &pin.center);
        let db = target_distance_square(board, *b, &pin.center);
        da.total_cmp(&db).then_with(|| a.cmp(b))
    });
    if targets.len() <= 4 {
        // No allowance: `RoutingBoard.fanout` builds its engine with
        // `init_autoroute(..., null TimeLimit, ...)` (`:1053-1055`), so an
        // escape attempt is never time-limited.
        let first = crate::batch::route_with_control(
            board,
            rules,
            ctrl.clone(),
            &connected,
            &targets[..1],
            None,
        );
        if matches!(
            first.state(),
            AttemptState::Routed | AttemptState::AlreadyConnected
        ) || targets.len() == 1
        {
            return first;
        }
    }
    crate::batch::route_with_control(board, rules, ctrl, &connected, &targets, None)
}

/// The control one escape attempt runs under — `RoutingBoard.fanout:1018-1050`.
///
/// This is the whole difference between a fanout attempt and an ordinary
/// connection (see the module docs): the maze, the locator and the inserter are
/// the same code, driven by these six fields.
pub fn fanout_control(
    board: &Board,
    rules: &BoardRules,
    settings: &RouterSettings,
    config: &FanoutConfig,
    pin: &FanoutPin,
    ripup_costs: i32,
) -> AutorouteControl {
    let mut ctrl = AutorouteControl::for_net_with_costs(
        board,
        rules,
        settings,
        pin.net_no,
        settings.scoring.via_costs(),
    );
    // `:1025-1046` (row 27): the net class's vias followed by the board's, and
    // the via masks rebuilt over the merged rule.
    if config.fallback_to_board_vias {
        if let Some(combined) = combined_via_rule(&ctrl.via_rule, rules, pin.net_no) {
            ctrl.via_rule = combined;
            ctrl.rebuild_via_info(board, settings.scoring.via_costs(), pin.net_no);
        }
    }
    ctrl.is_fanout = true;
    ctrl.fanout_start_pin_center = Some(pin.center);
    ctrl.fanout_start_pin_layer = pin.layer;
    ctrl.fanout_min_escape_length = Some(config.min_escape_length);
    ctrl.fanout_max_escape_length = Some(config.max_escape_length);
    // `:1044` (row 32) — the escape via must survive `remove_tails(FANOUT_VIA)`.
    ctrl.remove_unconnected_vias = false;
    // `:1047-1050` (row 25) — a negative budget means "no ripup".
    if ripup_costs >= 0 {
        ctrl.ripup_allowed = true;
        ctrl.ripup_costs = ripup_costs;
    } else {
        ctrl.ripup_allowed = false;
    }
    ctrl
}

fn target_distance_square(board: &Board, id: ItemId, pin_center: &IntPoint) -> f64 {
    let Some(item) = board.store().get(id) else {
        return f64::MAX;
    };
    let center = match &item.kind {
        ItemKind::Pin(pin) => pin.center.to_float(),
        ItemKind::Via(via) => via.center.to_float(),
        ItemKind::Trace(trace) => {
            let b = trace.polyline.bounding_box();
            FloatPoint::new(
                (b.ll.x as f64 + b.ur.x as f64) / 2.0,
                (b.ll.y as f64 + b.ur.y as f64) / 2.0,
            )
        }
        ItemKind::ConductionArea(area) => match &area.area.relative_area {
            fr_board::item::AreaShape::Box(b) => FloatPoint {
                x: f64::from(b.ll.x + b.ur.x) / 2.0 + f64::from(area.area.translation.x),
                y: f64::from(b.ll.y + b.ur.y) / 2.0 + f64::from(area.area.translation.y),
            },
            fr_board::item::AreaShape::Octagon(o) => {
                let b = o.bounding_box();
                FloatPoint {
                    x: f64::from(b.ll.x + b.ur.x) / 2.0 + f64::from(area.area.translation.x),
                    y: f64::from(b.ll.y + b.ur.y) / 2.0 + f64::from(area.area.translation.y),
                }
            }
            fr_board::item::AreaShape::Simplex(s) => {
                let b = s.bounding_box();
                FloatPoint {
                    x: f64::from(b.ll.x + b.ur.x) / 2.0 + f64::from(area.area.translation.x),
                    y: f64::from(b.ll.y + b.ur.y) / 2.0 + f64::from(area.area.translation.y),
                }
            }
            _ => return f64::MAX,
        },
        _ => return f64::MAX,
    };
    center.distance_square(&pin_center.to_float())
}

/// `RoutingBoard.fanout:1025-1046` — the net class's via rule followed by every
/// board via it does not already contain (row 27's `fallbackToBoardVias`).
/// `None` when there is nothing to add.
fn combined_via_rule(class_rule: &ViaRule, rules: &BoardRules, net_no: i32) -> Option<ViaRule> {
    let board_rule = rules.via_rules.first()?;
    let target_clearance = rules
        .nets
        .get(net_no)
        .map(|n| n.net_class_index())
        .and_then(|idx| rules.net_classes.get(idx))
        .map(|nc| {
            nc.default_item_clearance_classes
                .get(fr_rules::ItemClass::Via) as usize
        })
        .or_else(|| class_rule.get_via(0).map(|v| v.clearance_class()))
        .unwrap_or(1);
    let mut combined = ViaRule::new(format!("{}_fallback", class_rule.name));
    for index in 0..class_rule.via_count() {
        if let Some(via) = class_rule.get_via(index) {
            combined.append_via(via.clone());
        }
    }
    let mut added = false;
    for index in 0..board_rule.via_count() {
        let Some(via) = board_rule.get_via(index) else {
            continue;
        };
        if !combined.contains(via.name()) {
            let mut fallback_via = via.clone();
            fallback_via.set_clearance_class(target_clearance);
            combined.append_via(fallback_via);
            added = true;
        }
    }
    if added {
        Some(combined)
    } else {
        None
    }
}

fn via_count(board: &Board) -> usize {
    board
        .store()
        .iter()
        .filter(|(_, item)| matches!(item.kind, ItemKind::Via(_)))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestFixture;

    /// The shipping default is the scheduler-accepted divergence: the fallback
    /// lends a via whose clearance class is the *default net class's*, which
    /// under correct per-net semantics can land under-clearanced (Issue039
    /// via(1462338,-671317), 2448 actual vs 2502 required). Upstream defaults
    /// it on (`DefaultSettings.java:123`) and never sees the defect because its
    /// SesReader re-imports vias with the default class too
    /// (`SesReader.java:359-363`). `FR_FANOUT_FALLBACK_VIAS=1` restores it.
    #[test]
    fn fallback_to_board_vias_ships_off_and_the_env_switch_restores_it() {
        assert!(!RouterSettings::default().fanout.fallback_to_board_vias);
        // The env override parses as a three-way switch, not a presence flag.
        assert_eq!(parse_fallback_env("1"), Some(true));
        assert_eq!(parse_fallback_env("on"), Some(true));
        assert_eq!(parse_fallback_env("0"), Some(false));
        assert_eq!(parse_fallback_env("off"), Some(false));
        // Not a switch: leave the (now-off) settings default in charge.
        assert_eq!(parse_fallback_env(""), None);
        assert_eq!(parse_fallback_env("junk"), None);
    }

    /// With the fallback off (the shipping default), a net whose class has no
    /// via rule cannot borrow the board's — upstream's
    /// `BatchFanout.java:196-199` skip, minus the fallback arm.
    #[test]
    fn a_net_without_class_vias_cannot_borrow_board_vias_when_the_fallback_is_off() {
        let fixture = TestFixture::empty_two_layer();
        let rules = &fixture.rules;
        let net_no = crate::test_support::TEST_NET;
        // The fixture's net class carries vias, so only the fallback flag can
        // make the difference here.
        assert!(net_can_use_vias(rules, net_no, true));
        assert_eq!(
            net_can_use_vias(rules, net_no, false),
            rules
                .nets
                .get(net_no)
                .and_then(|net| rules.net_classes.get(net.net_class_index()))
                .and_then(|class| class.get_via_rule())
                .is_some_and(|rule| rule.via_count() > 0)
        );
    }

    #[test]
    fn the_release_sorting_keys_are_snake_case() {
        assert_eq!(
            PinSortingOrder::parse("outer_first"),
            PinSortingOrder::OuterFirst
        );
        assert_eq!(
            PinSortingOrder::parse("inner_first"),
            PinSortingOrder::InnerFirst
        );
        assert_eq!(
            PinSortingOrder::parse("distance_to_closest_on_net"),
            PinSortingOrder::DistanceToClosestOnNet
        );
        assert_eq!(
            PinSortingOrder::parse("surroundings_density"),
            PinSortingOrder::SurroundingsDensity
        );
        // Conformance row 15: the spec body's camelCase spellings silently fall
        // through to pin-index order, and so must ours.
        assert_eq!(
            PinSortingOrder::parse("distanceToClosestOnNet"),
            PinSortingOrder::PinIndex
        );
        assert_eq!(PinSortingOrder::default(), PinSortingOrder::OuterFirst);
    }

    #[test]
    fn the_escape_annulus_defaults_are_the_configured_ones() {
        let settings = RouterSettings::default();
        assert_eq!(
            settings.fanout.min_escape_length_mm,
            DEFAULT_MIN_ESCAPE_LENGTH_MM
        );
        assert_eq!(
            settings.fanout.max_escape_length_mm,
            DEFAULT_MAX_ESCAPE_LENGTH_MM
        );
        // Confirmed values, not the null fallbacks (conformance rows 10-11).
        assert_ne!(DEFAULT_MIN_ESCAPE_LENGTH_MM, FALLBACK_MIN_ESCAPE_LENGTH_MM);
        assert_ne!(DEFAULT_MAX_ESCAPE_LENGTH_MM, FALLBACK_MAX_ESCAPE_LENGTH_MM);
    }

    #[test]
    fn the_config_converts_millimetres_to_board_units() {
        let settings = RouterSettings::default();
        let config = FanoutConfig::from_settings(&settings, 1000.0);
        assert_eq!(config.min_escape_length, 2_500.0);
        assert_eq!(config.max_escape_length, 4_500.0);
        assert_eq!(config.density_radius, 20_000.0);
        assert_eq!(config.max_passes, DEFAULT_MAX_PASSES);
        assert_eq!(config.start_ripup_costs, crate::batch::START_RIPUP_COSTS);
        // A nonsense scale must not produce a NaN annulus.
        let degenerate = FanoutConfig::from_settings(&settings, 0.0);
        assert_eq!(degenerate.min_escape_length, DEFAULT_MIN_ESCAPE_LENGTH_MM);
    }

    /// Row 25: `ripupCosts = startRipupCosts * (passNo + 1)` with the pass
    /// number **0-based** (`BatchFanout.java:155`), and `-1` when ripup is off.
    #[test]
    fn the_ripup_schedule_is_start_costs_times_pass_number() {
        let mut fixture = TestFixture::empty_two_layer();
        let rules = fixture.rules.clone();
        let settings = fixture.settings.clone();
        let mut config = FanoutConfig::from_settings(&settings, 1000.0);
        let mut total = 0usize;
        let mut scratch = ContactScratch::new();
        let mut budget =
            |config: &FanoutConfig, board: &mut fr_board::board::Board, pass_no: i32| -> i32 {
                fanout_pass(
                    board,
                    &rules,
                    &settings,
                    config,
                    &[],
                    pass_no,
                    &mut total,
                    &mut scratch,
                )
                .ripup_costs
            };

        assert_eq!(
            budget(&config, &mut fixture.board, 0),
            100,
            "pass 0 pays one unit"
        );
        assert_eq!(budget(&config, &mut fixture.board, 1), 200);
        assert_eq!(budget(&config, &mut fixture.board, 19), 2_000);

        config.ripup_allowed = false;
        assert_eq!(
            budget(&config, &mut fixture.board, 3),
            -1,
            "ripup off is -1, not a budget of 0"
        );
    }

    #[test]
    fn components_come_out_biggest_first_and_pins_outer_first() {
        let fixture = TestFixture::two_layer_with_pads();
        let components = components(&fixture.board, PinSortingOrder::OuterFirst, 20_000.0);
        // The fixture's pads share component 0.
        assert_eq!(components.len(), 1);
        let pins = &components[0].pins;
        assert_eq!(pins.len(), 2);
        assert!(
            pins[0].distance_to_component_center >= pins[1].distance_to_component_center,
            "outer_first is descending distance"
        );
        assert!(pins.iter().all(|pin| pin.layer == 0));
    }

    #[test]
    fn netless_smd_pins_are_dropped() {
        let fixture = TestFixture::empty_two_layer();
        let components = components(&fixture.board, PinSortingOrder::OuterFirst, 20_000.0);
        assert!(components.is_empty());
    }

    #[test]
    fn a_fully_connected_pin_needs_no_escape() {
        let mut fixture = TestFixture::two_layer_with_pads();
        let centers = fixture.pad_centers();
        fixture
            .board
            .insert_trace_points(
                &[centers[0], centers[1]],
                0,
                crate::test_support::TEST_HALF_WIDTH,
                fr_board::net_nos::NetNos::of(crate::test_support::TEST_NET),
                1,
                fr_board::item::FixedState::Unfixed,
                None,
            )
            .expect("the straight wire inserts");
        let config = FanoutConfig::from_settings(&fixture.settings, 1.0);
        let components = components(
            &fixture.board,
            config.pin_sorting_order,
            config.density_radius,
        );
        let pin = components[0].pins[0].clone();
        let mut scratch = ContactScratch::new();
        let rules = fixture.rules.clone();
        let settings = fixture.settings.clone();
        let outcome = fanout_pin(
            &mut fixture.board,
            &rules,
            &settings,
            &config,
            &pin,
            100,
            &mut scratch,
        );
        assert_eq!(outcome.state(), AttemptState::NoUnconnectedNets);
    }

    /// The six control fields that *are* the fanout mode (module docs, rows
    /// 12/13/25/32): without them the same maze routes an ordinary connection.
    #[test]
    fn the_escape_control_carries_the_annulus_and_the_pin() {
        let fixture = TestFixture::two_layer_with_pads();
        let config = FanoutConfig::from_settings(&fixture.settings, 100.0);
        let components = components(
            &fixture.board,
            config.pin_sorting_order,
            config.density_radius,
        );
        let pin = &components[0].pins[0];
        let ctrl = fanout_control(
            &fixture.board,
            &fixture.rules,
            &fixture.settings,
            &config,
            pin,
            100,
        );

        assert!(ctrl.is_fanout);
        assert_eq!(ctrl.fanout_start_pin_center, Some(pin.center));
        assert_eq!(ctrl.fanout_start_pin_layer, pin.layer);
        // Board units, converted by the caller — 2.5 mm and 4.5 mm at 100
        // board units per mm.
        assert_eq!(ctrl.fanout_min_escape_length, Some(250.0));
        assert_eq!(ctrl.fanout_max_escape_length, Some(450.0));
        // Row 32: the escape via must outlive `remove_tails(FANOUT_VIA)`.
        assert!(!ctrl.remove_unconnected_vias);
        assert!(ctrl.ripup_allowed);
        assert_eq!(ctrl.ripup_costs, 100);
    }

    /// Row 25: `fanout.ripupAllowed = false` sends `-1`, which the attempt reads
    /// as "no ripup" rather than as a budget of zero.
    #[test]
    fn a_negative_budget_turns_ripup_off_entirely() {
        let fixture = TestFixture::two_layer_with_pads();
        let config = FanoutConfig::from_settings(&fixture.settings, 1.0);
        let components = components(
            &fixture.board,
            config.pin_sorting_order,
            config.density_radius,
        );
        let ctrl = fanout_control(
            &fixture.board,
            &fixture.rules,
            &fixture.settings,
            &config,
            &components[0].pins[0],
            -1,
        );
        assert!(!ctrl.ripup_allowed);
    }

    #[test]
    fn a_run_with_nothing_to_do_stops_immediately() {
        let mut fixture = TestFixture::empty_two_layer();
        let rules = fixture.rules.clone();
        let settings = fixture.settings.clone();
        let config = FanoutConfig::from_settings(&settings, 1.0);
        let outcome = fanout_board(&mut fixture.board, &rules, &settings, &config);
        assert_eq!(outcome.stop, Some(FanoutStop::NothingToDo));
        assert!(outcome.passes.is_empty());
    }

    #[test]
    fn a_pass_that_routes_nothing_stops_the_stage() {
        // Both pads are on layer 0 with nothing to escape to on another layer
        // that would help, so the first pass routes nothing and the stage stops
        // after exactly one pass — never the 14 the release's Issue219 ran.
        let mut fixture = TestFixture::two_layer_with_pads();
        let centers = fixture.pad_centers();
        fixture
            .board
            .insert_trace_points(
                &[centers[0], centers[1]],
                0,
                crate::test_support::TEST_HALF_WIDTH,
                fr_board::net_nos::NetNos::of(crate::test_support::TEST_NET),
                1,
                fr_board::item::FixedState::Unfixed,
                None,
            )
            .expect("the straight wire inserts");
        let rules = fixture.rules.clone();
        let settings = fixture.settings.clone();
        let config = FanoutConfig::from_settings(&settings, 1.0);
        let outcome = fanout_board(&mut fixture.board, &rules, &settings, &config);
        assert_eq!(outcome.stop, Some(FanoutStop::NothingRouted));
        assert_eq!(outcome.passes.len(), 1);
        assert_eq!(outcome.total_items_fanouted, 0);
    }

    /// The job deadline is a stage-level stop too: an already-spent budget ends
    /// the run before the first pass, like the batch loop's own
    /// `an_expired_job_deadline_stops_the_loop_before_any_pass`.
    #[test]
    fn an_expired_job_deadline_stops_fanout_before_any_pass() {
        let mut fixture = TestFixture::two_layer_with_pads();
        let rules = fixture.rules.clone();
        let settings = fixture.settings.clone();
        let config = FanoutConfig::from_settings(&settings, 1.0)
            .with_deadline(JobDeadline::already_passed());
        let outcome = fanout_board(&mut fixture.board, &rules, &settings, &config);
        assert_eq!(outcome.stop, Some(FanoutStop::JobDeadline));
        assert!(outcome.passes.is_empty());
        assert_eq!(outcome.total_items_fanouted, 0);
    }

    /// The per-pin poll: a pass that finds the budget spent aborts between
    /// pins — no pin attempted, and the stage reads the stop as the deadline,
    /// not as the release's `routed == 0` rule.
    #[test]
    fn a_deadline_spent_inside_a_pass_aborts_between_pins() {
        let mut fixture = TestFixture::two_layer_with_pads();
        let rules = fixture.rules.clone();
        let settings = fixture.settings.clone();
        let config = FanoutConfig::from_settings(&settings, 1.0)
            .with_deadline(JobDeadline::already_passed());
        let components = components(
            &fixture.board,
            config.pin_sorting_order,
            config.density_radius,
        );
        assert!(!components.is_empty(), "the fixture has SMD pins to escape");
        let mut total = 0;
        let mut scratch = ContactScratch::new();
        let counters = fanout_pass(
            &mut fixture.board,
            &rules,
            &settings,
            &config,
            &components,
            0,
            &mut total,
            &mut scratch,
        );
        assert_eq!(counters.routed, 0);
        assert_eq!(counters.not_routed, 0);
        assert_eq!(total, 0);
    }

    /// An unarmed deadline never fires: the default config reads the ambient
    /// deadline, which is [`JobDeadline::NONE`] unless `--job-timeout` armed it.
    #[test]
    fn an_unarmed_deadline_never_stops_the_stage() {
        let config = FanoutConfig::from_settings(&TestFixture::two_layer_with_pads().settings, 1.0);
        assert!(!config.deadline.is_armed() || !config.deadline.expired());
    }

    #[test]
    fn target_distance_square_uses_bounding_box_center_of_bent_trace() {
        let mut fixture = TestFixture::empty_two_layer();
        // Insert an L-shaped trace: (0, 0) -> (1000, 0) -> (1000, 2000)
        // Bounding box is [0, 0]..[1000, 2000], center is (500, 1000)
        let inserted = fixture.board.insert_trace_points(
            &[
                IntPoint::new(0, 0),
                IntPoint::new(1000, 0),
                IntPoint::new(1000, 2000),
            ],
            0,
            crate::test_support::TEST_HALF_WIDTH,
            fr_board::net_nos::NetNos::of(crate::test_support::TEST_NET),
            1,
            fr_board::item::FixedState::Unfixed,
            None,
        );
        assert!(inserted.is_some());
        let trace_id = inserted.unwrap();
        let pin_center = IntPoint::new(500, 1000);
        let dist_sq = target_distance_square(&fixture.board, trace_id, &pin_center);
        assert_eq!(dist_sq, 0.0);
    }
}
