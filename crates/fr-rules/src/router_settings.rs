//! Router knobs and the board-geometry-derived cost preparation.
//!
//! Ported from `app.freerouting.settings.RouterSettings` plus its four sub-objects
//! (`LayerSettings`, `ScoringSettings`, `FanoutSettings`, `OptimizerSettings`) and the hard-coded
//! defaults in `app.freerouting.settings.sources.DefaultSettings`.
//!
//! Specs: `docs/upstream-study/v2-vs-v19-and-scope.md` §3 (the knob checklist and defaults),
//! §5 (`applyBoardSpecificOptimizations`), §3.6 (the engine-side constants that are *not* knobs).
//!
//! # Design notes
//!
//! * Fields whose "absent" state has an observable, *different* meaning from their default are
//!   modelled as `Option`. Upstream needs every field boxed and nullable so that
//!   `ReflectionUtil.copyFields` can distinguish "this source has no opinion" from "this source
//!   set the Java default" (`v2-vs-v19-and-scope.md` §4); the same nullability leaks into the
//!   getters, several of which have a fallback that differs from `DefaultSettings`. Those
//!   divergent fallbacks are reproduced (see [`RouterSettings::run_fanout`] vs
//!   [`RouterSettings::is_fanout_enabled`], and [`ScoringSettings::via_costs`]).
//! * The settings *merge* pipeline (priorities, `copyFields`, env/CLI sources) is I/O-layer
//!   concern and lives outside this crate; [`RouterSettings::validate`] — upstream's only
//!   normalization step — is here because it is pure.

use crate::layer::LayerStructure;

/// Selector token for the shipping router (`RouterSettings.ALGORITHM_CURRENT`,
/// `RouterSettings.java:14`).
pub const ALGORITHM_CURRENT: &str = "freerouting-router";

/// Reserved selector for the v1.9 re-implementation. Reachable from the GUI only; the headless
/// entry point hard-codes `BatchAutorouter` (`batch-orchestration.md` conformance §1.1).
pub const ALGORITHM_V19: &str = "freerouting-router-v19";

/// Lower clamp for a per-layer bend cost (`RouterSettings.MIN_BEND_COST`, `RouterSettings.java:16`).
pub const MIN_BEND_COST: f64 = 0.0;

/// Upper clamp for a per-layer bend cost (`RouterSettings.MAX_BEND_COST`, `RouterSettings.java:17`).
pub const MAX_BEND_COST: f64 = 9.9;

/// `max_passes` values outside `0..=9999` fall back to this (`RouterSettings.java:932-941`).
pub const MAX_PASSES_LIMIT: i32 = 9999;

/// `trace_pull_tight_accuracy` fallback when the configured value is `< 1`.
pub const DEFAULT_TRACE_PULL_TIGHT_ACCURACY: i32 = 500;

// ---------------------------------------------------------------------------------------------
// Sub-settings
// ---------------------------------------------------------------------------------------------

/// Per-layer router settings (`settings/LayerSettings.java`).
///
/// All three fields are nullable upstream and their null-fallbacks differ, so they stay `Option`
/// here — `applyBoardSpecificOptimizations` keys off exactly this nullness to decide whether a
/// value was user-supplied.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LayerSettings {
    /// Whether the autorouter may use this layer. `None` reads as `true`; non-signal layers are
    /// force-set to `false` by [`RouterSettings::apply_board_specific_optimizations`].
    pub routable: Option<bool>,
    /// Preferred trace direction. `None` reads as `layer % 2 == 1`
    /// (`RouterSettings.get_preferred_direction_is_horizontal`, `RouterSettings.java:609-618`).
    pub preferred_direction_horizontal: Option<bool>,
    /// Per-layer bend cost, clamped to `[MIN_BEND_COST, MAX_BEND_COST]` on set and on
    /// default-read. `None` reads as `scoring.default_bend_cost`. **v2 only.**
    pub bend_cost: Option<f64>,
}

/// Scoring weights and per-layer trace costs (`settings/ScoringSettings.java`).
///
/// Defaults are the `DEFAULT_*` constants of `settings/sources/DefaultSettings.java`, which that
/// file declares to be "the single authoritative source of truth for every scoring weight".
#[derive(Debug, Clone, PartialEq)]
pub struct ScoringSettings {
    /// Per-layer cost of 1 mm of trace routed in the preferred direction. Derived, see
    /// [`RouterSettings::apply_board_specific_optimizations`].
    pub preferred_direction_trace_cost: Vec<f64>,
    /// Per-layer cost of 1 mm of trace routed against the preferred direction. Derived.
    pub undesired_direction_trace_cost: Vec<f64>,
    /// Scalar seed for `preferred_direction_trace_cost`. Default `1.0`
    /// (`DefaultSettings.DEFAULT_PREFERRED_DIRECTION_TRACE_COST`). Also the trace-length
    /// coefficient in `calculateScore`.
    pub default_preferred_direction_trace_cost: Option<f64>,
    /// Scalar seed for `undesired_direction_trace_cost`, *before* the aspect-ratio add-on.
    /// Default `1.0` (`DefaultSettings.DEFAULT_UNDESIRED_DIRECTION_TRACE_COST`).
    pub default_undesired_direction_trace_cost: Option<f64>,
    /// Maze via cost base and score cost term. Default `50`
    /// (`DefaultSettings.DEFAULT_VIA_COSTS`); reads back as `1` when unset.
    pub via_costs: Option<i32>,
    /// Cost of a via landing on a plane. Default `5`
    /// (`DefaultSettings.DEFAULT_PLANE_VIA_COSTS`); reads back as `1` when unset.
    pub plane_via_costs: Option<i32>,
    /// Base ripup cost, multiplied by the (1-based) pass number in `BatchAutorouter`. Default
    /// **`100`** (`DefaultSettings.DEFAULT_START_RIPUP_COSTS = 100`, applied at
    /// `DefaultSettings.java:147`); reads back as `1` when unset. Routing-control only — it does
    /// not appear in the board score. See `maze-engine.md` conformance row D5: the
    /// `AutorouteControl` ctor default of `ripup_costs = 1000` is always overwritten by
    /// `start_ripup_costs * ripup_pass_no`.
    pub start_ripup_costs: Option<i32>,
    /// Score penalty per incomplete connection, and the score normalizer. Default `5_000_000.0`.
    pub unrouted_net_penalty: f32,
    /// Score penalty per DRC violation. Default `1_000_000.0`.
    pub clearance_violation_penalty: f32,
    /// Score penalty per trace corner (score only, not the maze). Default `10.0`.
    pub bend_penalty: f32,
    /// Fallback for `layers[i].bend_cost`. Default `0.0`.
    pub default_bend_cost: Option<f64>,
}

impl Default for ScoringSettings {
    /// The `DefaultSettings` row for `router.scoring.*`. Per-layer arrays are intentionally empty:
    /// their length must come from the board, and `DefaultSettings` explicitly refuses to guess a
    /// size (`DefaultSettings.java:83-88`).
    fn default() -> Self {
        ScoringSettings {
            preferred_direction_trace_cost: Vec::new(),
            undesired_direction_trace_cost: Vec::new(),
            default_preferred_direction_trace_cost: Some(1.0),
            default_undesired_direction_trace_cost: Some(1.0),
            via_costs: Some(50),
            plane_via_costs: Some(5),
            start_ripup_costs: Some(100),
            unrouted_net_penalty: 5_000_000.0,
            clearance_violation_penalty: 1_000_000.0,
            bend_penalty: 10.0,
            default_bend_cost: Some(0.0),
        }
    }
}

impl ScoringSettings {
    /// `RouterSettings.get_via_costs` (`RouterSettings.java:530-532`): falls back to `1`, not to
    /// the `DefaultSettings` value of 50, when the field is unset.
    pub fn via_costs(&self) -> i32 {
        self.via_costs.unwrap_or(1)
    }

    /// `RouterSettings.set_via_costs` (`RouterSettings.java:534-539`): clamped to `>= 1`.
    pub fn set_via_costs(&mut self, value: i32) {
        self.via_costs = Some(value.max(1));
    }

    /// `RouterSettings.get_plane_via_costs` (`RouterSettings.java:541-543`): falls back to `1`.
    pub fn plane_via_costs(&self) -> i32 {
        self.plane_via_costs.unwrap_or(1)
    }

    /// `RouterSettings.set_plane_via_costs` (`RouterSettings.java:545-550`): clamped to `>= 1`.
    pub fn set_plane_via_costs(&mut self, value: i32) {
        self.plane_via_costs = Some(value.max(1));
    }

    /// `RouterSettings.get_start_ripup_costs` (`RouterSettings.java:473-475`): falls back to `1`.
    pub fn start_ripup_costs(&self) -> i32 {
        self.start_ripup_costs.unwrap_or(1)
    }

    /// `RouterSettings.set_start_ripup_costs` (`RouterSettings.java:477-482`): clamped to `>= 1`.
    pub fn set_start_ripup_costs(&mut self, value: i32) {
        self.start_ripup_costs = Some(value.max(1));
    }
}

/// SMD pin-escape (fanout) pre-pass settings (`settings/FanoutSettings.java`).
#[derive(Debug, Clone, PartialEq)]
pub struct FanoutSettings {
    /// Run the fanout pre-pass. Default `true` (v1.9: `false`). Note the two different null
    /// policies upstream: [`RouterSettings::run_fanout`] treats `None` as `true`,
    /// [`RouterSettings::is_fanout_enabled`] treats `None` as `false`
    /// (`RouterSettings.java:503-511`).
    pub enabled: Option<bool>,
    /// Safety cap on fanout passes. Default `20`.
    pub max_passes: i32,
    /// Cap on escape attempts. Default `i32::MAX`.
    pub max_items: i32,
    /// Per-pin budget in pass 1; scales as `max_milliseconds_per_pin * pass_number`. Default
    /// `10_000`.
    pub max_milliseconds_per_pin: i64,
    /// Allow fanout to rip existing traces. Default `true`.
    pub ripup_allowed: bool,
    /// Minimum via distance from the pin centre, in mm. Default `2.5`.
    pub min_escape_length_mm: f64,
    /// Search / via-placement radius bound, in mm. Default `4.5`.
    pub max_escape_length_mm: f64,
    /// Via inside the pad, in mm. Default `0.250`.
    pub start_via_diameter_mm: f64,
    /// Landing via, in mm. Default `0.250`.
    pub end_via_diameter_mm: f64,
    /// `"outer_first"` (default), `"inner_first"` (= the v1.9 order) or `"unsorted"`.
    pub pin_sorting_order: String,
    /// Use board-wide via rules when the net's via list is empty.
    ///
    /// Upstream default is `true` (`DefaultSettings.java:123`), but shipping
    /// default here is **`false`** — a deliberate divergence (see
    /// `PARITY-DIVERGENCES.md`, "fanout fallbackToBoardVias"): the fallback
    /// lends a board-default via whose clearance class is the *default net
    /// class's*, not the routed net's, so under correct per-net clearance
    /// semantics the lent via can land under-clearanced (Issue039
    /// via(1462338,-671317): 2448 actual vs 2502 required on the Power net).
    /// Upstream never notices because its SesReader re-imports vias with the
    /// default class too (`SesReader.java:359-363`); our Lossless reader
    /// restores the net class and charges it. Re-enable per run with
    /// `FR_FANOUT_FALLBACK_VIAS=1` or a settings file with `true`.
    pub fallback_to_board_vias: bool,
    /// Stage timeout string; `None` = no stage timeout.
    pub timeout: Option<String>,
}

impl Default for FanoutSettings {
    /// The `DefaultSettings` block for `router.fanout.*` (`DefaultSettings.java:110-122`).
    fn default() -> Self {
        FanoutSettings {
            enabled: Some(true),
            max_passes: 20,
            max_items: i32::MAX,
            max_milliseconds_per_pin: 10_000,
            ripup_allowed: true,
            min_escape_length_mm: 2.5,
            max_escape_length_mm: 4.5,
            start_via_diameter_mm: 0.250,
            end_via_diameter_mm: 0.250,
            pin_sorting_order: "outer_first".to_string(),
            // Deliberate divergence from `DefaultSettings.java:123` (which sets
            // `true`): see the field docs and PARITY-DIVERGENCES.md.
            fallback_to_board_vias: false,
            timeout: None,
        }
    }
}

/// How the optimizer publishes an improved board (`autoroute/BoardUpdateStrategy.java`, CLI `-us`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BoardUpdateStrategy {
    /// Keep every improvement immediately. The default.
    #[default]
    Greedy,
    /// Only keep a pass that improves the global score.
    GlobalOptimal,
    /// Alternate the two according to `hybrid_ratio`.
    Hybrid,
}

/// How the optimizer picks the next item to re-route (`autoroute/ItemSelectionStrategy.java`,
/// CLI `-is`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ItemSelectionStrategy {
    /// Board order.
    Sequential,
    /// Uniform random.
    Random,
    /// Worst-scoring first. The default.
    #[default]
    Prioritized,
}

/// Post-route optimizer settings (`settings/OptimizerSettings.java`).
#[derive(Debug, Clone, PartialEq)]
pub struct OptimizerSettings {
    /// `DefaultSettings` sets this to `true`, but [`RouterSettings::run_optimizer`] returns
    /// `false` when it is unset (`RouterSettings.java:493-495`).
    pub enabled: Option<bool>,
    /// Selector token. Default `"freerouting-optimizer"`.
    pub algorithm: String,
    /// Full sweeps over items. Default `100`.
    pub max_passes: i32,
    /// Item cap. Default `i32::MAX`.
    pub max_items: i32,
    /// Kept in sync from [`RouterSettings::set_max_threads`]. Default `max(1, cores - 1)`.
    pub max_threads: usize,
    /// Fractional improvement threshold, default `0.01` (1 %). CLI `-oit` takes **percent** and
    /// divides by 100.
    pub improvement_threshold: f32,
    /// Abort the current pass after this many consecutive failures. Default `50`.
    pub max_consecutive_failures: i32,
    /// `ripup_costs *= factor` at optimizer start. Default `10`.
    pub additional_ripup_cost_factor_at_start: i32,
    /// `ripup_costs = round(factor * ripup_costs)` when re-routing a trace. Default `0.6`.
    pub trace_ripup_cost_factor: f32,
    /// Passes allowed when re-routing one item. Default `6`.
    pub max_autoroute_passes: i32,
    /// Board update strategy. Default [`BoardUpdateStrategy::Greedy`].
    pub board_update_strategy: BoardUpdateStrategy,
    /// Hybrid ratio string. Default `"1:1"`.
    pub hybrid_ratio: String,
    /// Item selection strategy. Default [`ItemSelectionStrategy::Prioritized`].
    pub item_selection_strategy: ItemSelectionStrategy,
    /// Stage timeout string; `None` = no stage timeout.
    pub timeout: Option<String>,
}

impl Default for OptimizerSettings {
    /// The `DefaultSettings` block for `router.optimizer.*` (`DefaultSettings.java:124-138`).
    fn default() -> Self {
        OptimizerSettings {
            enabled: Some(true),
            algorithm: "freerouting-optimizer".to_string(),
            max_passes: 100,
            max_items: i32::MAX,
            max_threads: default_max_threads(),
            improvement_threshold: 0.01,
            max_consecutive_failures: 50,
            additional_ripup_cost_factor_at_start: 10,
            trace_ripup_cost_factor: 0.6,
            max_autoroute_passes: 6,
            board_update_strategy: BoardUpdateStrategy::Greedy,
            hybrid_ratio: "1:1".to_string(),
            item_selection_strategy: ItemSelectionStrategy::Prioritized,
            timeout: None,
        }
    }
}

/// A per-layer `(horizontal, vertical)` maze expansion cost pair
/// (`autoroute/AutorouteControl.ExpansionCostFactor`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExpansionCostFactor {
    /// Cost of 1 mm routed horizontally on this layer.
    pub horizontal: f64,
    /// Cost of 1 mm routed vertically on this layer.
    pub vertical: f64,
}

// ---------------------------------------------------------------------------------------------
// Board geometry input
// ---------------------------------------------------------------------------------------------

/// The board bounding-box extents that drive
/// [`RouterSettings::apply_board_specific_optimizations`].
///
/// Upstream reads `board.bounding_box.width()` / `.height()` directly
/// (`RouterSettings.java:225-226`). Both extents are kept (rather than a single ratio) so that the
/// two add-on terms use the *same* division order as Java — `(10.0 * W) / H` — and so the seed
/// `W < H` stays an exact comparison.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoardAspectRatio {
    horizontal_width: f64,
    vertical_width: f64,
}

impl BoardAspectRatio {
    /// Builds the aspect input from a board bounding box.
    pub fn from_bounding_box(width: f64, height: f64) -> Self {
        BoardAspectRatio {
            horizontal_width: width,
            vertical_width: height,
        }
    }

    /// Board bounding-box width (`horizontal_width` upstream).
    pub fn horizontal_width(&self) -> f64 {
        self.horizontal_width
    }

    /// Board bounding-box height (`vertical_width` upstream).
    pub fn vertical_width(&self) -> f64 {
        self.vertical_width
    }

    /// `W / H`. Provided for reporting; the cost formulas below do not route through it, to keep
    /// the division order identical to Java.
    pub fn ratio(&self) -> f64 {
        self.horizontal_width / self.vertical_width
    }

    /// `0.1 * round(10 * W / H)` — the extra cost charged against the preferred direction on
    /// layers whose preferred direction is horizontal (`RouterSettings.java:251`).
    pub fn horizontal_add_costs_against_preferred_dir(&self) -> f64 {
        0.1 * java_math_round(10.0 * self.horizontal_width / self.vertical_width) as f64
    }

    /// `0.1 * round(10 * H / W)` — the same for vertically-preferred layers
    /// (`RouterSettings.java:253`).
    pub fn vertical_add_costs_against_preferred_dir(&self) -> f64 {
        0.1 * java_math_round(10.0 * self.vertical_width / self.horizontal_width) as f64
    }

    /// The seed for the alternating preferred-direction walk: `W < H`
    /// (`RouterSettings.java:257`, "make more horizontal preferred direction, if the board is
    /// horizontal"). It is flipped *before* the first assignment, so layer 0 — when it is a signal
    /// layer — receives `!(W < H)`.
    pub fn preferred_direction_is_horizontal_seed(&self) -> bool {
        self.horizontal_width < self.vertical_width
    }
}

/// `java.lang.Math.round(double)`: round half **up** (towards positive infinity), `NaN` ⇒ 0,
/// infinities ⇒ the saturating `i64` bounds.
///
/// Deliberately not `f64::round`, which rounds half *away from zero*: they disagree on negative
/// halves (`Math.round(-2.5) == -2`, `(-2.5f64).round() == -3.0`). The `floor`/`diff` form also
/// reproduces the Java 7+ fix for `0.49999999999999994`, where naive `floor(x + 0.5)` returns 1.
fn java_math_round(value: f64) -> i64 {
    if value.is_nan() {
        return 0;
    }
    let floor = value.floor();
    let result = if value - floor >= 0.5 {
        floor + 1.0
    } else {
        floor
    };
    // Rust's float-to-int `as` cast saturates, matching Math.round's clamping at the long bounds.
    result as i64
}

/// Number of logical processors, as `Runtime.getRuntime().availableProcessors()`.
fn available_processors() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

/// `RouterSettings.defaultMaxThreads()` (`RouterSettings.java:103-105`): `max(1, cores - 1)`.
pub fn default_max_threads() -> usize {
    available_processors().saturating_sub(1).max(1)
}

/// `RouterSettings.normalizeMaxThreads` (`RouterSettings.java:107-119`): `None`/negative ⇒
/// `cores - 1`; `0` ⇒ all cores; otherwise capped at the core count.
pub fn normalize_max_threads(value: Option<i32>) -> usize {
    match value {
        None => default_max_threads(),
        Some(v) if v < 0 => default_max_threads(),
        Some(0) => available_processors(),
        Some(v) => (v as usize).min(available_processors()),
    }
}

// ---------------------------------------------------------------------------------------------
// RouterSettings
// ---------------------------------------------------------------------------------------------

/// Top-level `router.*` settings (`settings/RouterSettings.java`).
///
/// [`RouterSettings::default`] reproduces `settings/sources/DefaultSettings.getSettings()` — the
/// priority-0 source that every other source merges on top of.
#[derive(Debug, Clone, PartialEq)]
pub struct RouterSettings {
    /// Run the autoroute stage at all. `None` reads as `true` ([`Self::run_router`]).
    pub enabled: Option<bool>,
    /// Router selector token. Default [`ALGORITHM_CURRENT`].
    pub algorithm: String,
    /// `"HH:MM:SS"` or `"5m"`. Default `"12:00:00"`; clamped to 24 h by the scheduler.
    pub job_timeout: String,
    /// Ripup-and-reroute pass cap. Default `9999`; see [`Self::validate`] for the `0` ⇒ unlimited
    /// rule.
    pub max_passes: i32,
    /// Hard cap on `routed + failed`. Default `i32::MAX`.
    pub max_items: i32,
    /// Worker threads. Default `max(1, cores - 1)`; mirrored into `optimizer.max_threads` by
    /// [`Self::set_max_threads`].
    pub max_threads: usize,
    /// Pull-tight `min_translate_dist` seed. Default `500`. Note `AutorouteControl` hard-codes its
    /// own `pullTightAccuracy = 500` and does **not** read this
    /// (`v2-vs-v19-and-scope.md` §3.6).
    pub trace_pull_tight_accuracy: i32,
    /// Whether the maze may change layers at all. Default `true`. (The JSON key
    /// `allowed_via_types` is a historical misnomer; it is a boolean.)
    pub vias_allowed: bool,
    /// Automatic trace neckdown at pins. Default `true`.
    pub automatic_neckdown: bool,
    /// Dump per-pass boards. Default `false`.
    pub save_intermediate_stages: bool,
    /// CLI `-inc a,b`. Default empty. **Dead in headless mode** — only the GUI translates it into
    /// `NetClass.is_ignored_by_autorouter`, so a CLI run never applies it
    /// (`v2-vs-v19-and-scope.md` §3.1).
    pub ignore_net_classes: Vec<String>,
    /// Copper-to-board-edge clearance in µm. Default `500.0`.
    pub copper_to_edge_clearance_um: f64,
    /// Drill-hole-to-copper clearance in µm. Default `0.0` (preserves legacy DSN behaviour).
    pub hole_clearance_um: f64,
    /// Opt-in width necking for a failed connection retry, in µm. Default `0.0` = off.
    pub neck_width_um: f64,
    /// Rip up a routed connection whose new items carry violations. Default `false`.
    pub strict_drc: bool,
    /// Path for the `RoutingResultManifest` JSON. Default `None`. (Upstream's `clone()` drops
    /// this field — a latent bug we do not reproduce, since we derive `Clone`.)
    pub result_json_path: Option<String>,
    /// Per-layer settings; length is the board layer count. Empty until the board is known —
    /// `DefaultSettings` deliberately refuses to guess a size (`DefaultSettings.java:105-107`).
    pub layers: Vec<LayerSettings>,
    /// Fanout pre-pass sub-settings.
    pub fanout: FanoutSettings,
    /// Optimizer sub-settings.
    pub optimizer: OptimizerSettings,
    /// Scoring weights and derived per-layer trace costs.
    pub scoring: ScoringSettings,

    /// `RouterSettings.boardSpecificTraceCostsApplied` (`RouterSettings.java:80`): once set, the
    /// per-layer trace costs are considered user-owned and are not recomputed.
    board_specific_trace_costs_applied: bool,
}

impl Default for RouterSettings {
    /// `settings/sources/DefaultSettings.getSettings()` (`DefaultSettings.java:81-155`).
    fn default() -> Self {
        RouterSettings {
            enabled: Some(true),
            algorithm: ALGORITHM_CURRENT.to_string(),
            job_timeout: "12:00:00".to_string(),
            max_passes: MAX_PASSES_LIMIT,
            max_items: i32::MAX,
            max_threads: default_max_threads(),
            trace_pull_tight_accuracy: DEFAULT_TRACE_PULL_TIGHT_ACCURACY,
            vias_allowed: true,
            automatic_neckdown: true,
            save_intermediate_stages: false,
            ignore_net_classes: Vec::new(),
            copper_to_edge_clearance_um: 500.0,
            hole_clearance_um: 0.0,
            neck_width_um: 0.0,
            strict_drc: false,
            result_json_path: None,
            layers: Vec::new(),
            fanout: FanoutSettings::default(),
            optimizer: OptimizerSettings::default(),
            scoring: ScoringSettings::default(),
            board_specific_trace_costs_applied: false,
        }
    }
}

impl RouterSettings {
    /// Number of layers the settings are currently sized for
    /// (`RouterSettings.getLayerCount`, `RouterSettings.java:373-379`).
    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }

    /// Whether the per-layer trace costs have already been computed or explicitly set
    /// (`RouterSettings.areBoardSpecificTraceCostsApplied`, `RouterSettings.java:220-222`).
    pub fn board_specific_trace_costs_applied(&self) -> bool {
        self.board_specific_trace_costs_applied
    }

    /// Run the autoroute stage. `None` ⇒ `true` (`RouterSettings.java:483-485`).
    pub fn run_router(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    /// Run the optimizer stage. `None` ⇒ **`false`** (`RouterSettings.java:493-495`), even though
    /// `DefaultSettings` sets it to `true`.
    pub fn run_optimizer(&self) -> bool {
        self.optimizer.enabled.unwrap_or(false)
    }

    /// `getRunFanout()`: `None` ⇒ **`true`** (`RouterSettings.java:503-505`).
    ///
    /// Deliberately different from [`Self::is_fanout_enabled`]; both null policies ship in the
    /// same class.
    pub fn run_fanout(&self) -> bool {
        self.fanout.enabled.unwrap_or(true)
    }

    /// `isFanoutEnabled()`: `None` ⇒ **`false`** (`RouterSettings.java:510-512`). This is the one
    /// the headless entry point actually gates the fanout pre-pass on.
    pub fn is_fanout_enabled(&self) -> bool {
        self.fanout.enabled.unwrap_or(false)
    }

    /// Sets `max_threads` through `normalizeMaxThreads` and mirrors the result into
    /// `optimizer.max_threads`, as the upstream setter does (`RouterSettings.java:144-154`).
    pub fn set_max_threads(&mut self, value: Option<i32>) {
        self.max_threads = normalize_max_threads(value);
        self.optimizer.max_threads = self.max_threads;
    }

    /// Sets the layer count and (re-)initializes the per-layer settings and cost arrays.
    ///
    /// `RouterSettings.setLayerCount` (`RouterSettings.java:389-418`). Note that it resets
    /// `routable` to `true`, clears `preferred_direction_horizontal` and `bend_cost`, and refills
    /// both cost arrays with `1.0` for **every** layer — even when the length was already correct
    /// — while `board_specific_trace_costs_applied` is only cleared when the length changed.
    pub fn set_layer_count(&mut self, layer_count: usize) {
        if self.layers.len() != layer_count {
            self.board_specific_trace_costs_applied = false;
            self.layers = vec![LayerSettings::default(); layer_count];
        }
        self.scoring.preferred_direction_trace_cost = vec![1.0; layer_count];
        self.scoring.undesired_direction_trace_cost = vec![1.0; layer_count];
        for layer in &mut self.layers {
            layer.routable = Some(true);
            layer.preferred_direction_horizontal = None;
            layer.bend_cost = None;
        }
    }

    /// Re-runs [`Self::apply_board_specific_optimizations`] only when the layer count changed or
    /// the costs were never applied (`RouterSettings.applyBoardSpecificOptimizationsIfNeeded`,
    /// `RouterSettings.java:207-215`).
    pub fn apply_board_specific_optimizations_if_needed(
        &mut self,
        aspect: BoardAspectRatio,
        layer_structure: &LayerStructure,
    ) {
        if self.layer_count() != layer_structure.len() || !self.board_specific_trace_costs_applied {
            self.apply_board_specific_optimizations(aspect, layer_structure);
        }
    }

    /// Derives the per-layer preferred directions and trace costs from the board geometry.
    ///
    /// Transcribed from `RouterSettings.applyBoardSpecificOptimizations`
    /// (`RouterSettings.java:224-336`), which is arithmetically identical to
    /// v1.9's `AutorouteSettings(RoutingBoard)`. Spec: `v2-vs-v19-and-scope.md` §5.
    ///
    /// ```text
    /// horizAdd = 0.1 * round(10 * W / H)                       // :251
    /// vertAdd  = 0.1 * round(10 * H / W)                       // :253
    /// prefIsHorizontal = (W < H)                               // :257  (seed)
    /// for i in 0..layerCount:
    ///     if !signal[i]:            layers[i].routable = false // :300-301
    ///     elif routable == null:    layers[i].routable = true  // :302-303
    ///     if bendCost == null:      bendCost = scoring.defaultBendCost ?? 0.0   // :305-307
    ///     if signal[i]:             prefIsHorizontal = !prefIsHorizontal        // :308-310
    ///     if preferredDirectionHorizontal == null: = prefIsHorizontal           // :311-313
    ///     if initializeTraceCosts:                                              // :315-323
    ///         pref[i]      = scoring.defaultPreferredDirectionTraceCost
    ///         undesired[i] = scoring.defaultUndesiredDirectionTraceCost
    ///                      + (prefIsHorizontal ? horizAdd : vertAdd)
    /// if initializeTraceCosts and signalLayerCount > 2:                         // :325-334
    ///     outerAdd = 0.2 * signalLayerCount
    ///     pref[0] += outerAdd; pref[last] += outerAdd
    ///     undesired[0] += outerAdd; undesired[last] += outerAdd
    /// boardSpecificTraceCostsApplied = true                                     // :335
    /// ```
    ///
    /// Three details that are easy to lose:
    ///
    /// * The **flip happens before the assignment** (`:308-310` precedes `:311-313`), so a signal
    ///   layer 0 gets `!(W < H)`, not `W < H`. Non-signal layers do not flip, so a power plane in
    ///   the middle of the stack does not consume a direction slot.
    /// * The outer-layer surcharge uses **`layer_count - 1`**, i.e. the last *board* layer, while
    ///   the `> 2` test and the `0.2 *` factor use the **signal** layer count.
    /// * Reshaping `layers` or either cost array clears
    ///   `board_specific_trace_costs_applied`, which is what makes a re-run recompute the costs
    ///   (`:260-286`).
    pub fn apply_board_specific_optimizations(
        &mut self,
        aspect: BoardAspectRatio,
        layer_structure: &LayerStructure,
    ) {
        let layer_count = layer_structure.len();

        let horizontal_add_costs = aspect.horizontal_add_costs_against_preferred_dir();
        let vertical_add_costs = aspect.vertical_add_costs_against_preferred_dir();
        let mut curr_preferred_direction_is_horizontal =
            aspect.preferred_direction_is_horizontal_seed();

        // Initialize the layer specific settings, keeping any pre-existing entry (:260-278).
        if self.layers.len() != layer_count {
            self.board_specific_trace_costs_applied = false;
            let old_layers = std::mem::take(&mut self.layers);
            let mut old_iter = old_layers.into_iter();
            self.layers = (0..layer_count)
                .map(|_| old_iter.next().unwrap_or_default())
                .collect();
        }
        if self.scoring.preferred_direction_trace_cost.len() != layer_count {
            self.scoring.preferred_direction_trace_cost = vec![0.0; layer_count];
            self.board_specific_trace_costs_applied = false;
        }
        if self.scoring.undesired_direction_trace_cost.len() != layer_count {
            self.scoring.undesired_direction_trace_cost = vec![0.0; layer_count];
            self.board_specific_trace_costs_applied = false;
        }

        // Guard against absent defaults on the deserialization path (:288-295).
        let default_preferred = *self
            .scoring
            .default_preferred_direction_trace_cost
            .get_or_insert(1.0);
        let default_undesired = *self
            .scoring
            .default_undesired_direction_trace_cost
            .get_or_insert(1.0);
        let default_bend_cost = self.scoring.default_bend_cost.unwrap_or(0.0);

        let initialize_trace_costs = !self.board_specific_trace_costs_applied;

        for i in 0..layer_count {
            let is_signal = layer_structure.arr[i].is_signal;
            if !is_signal {
                self.layers[i].routable = Some(false);
            } else if self.layers[i].routable.is_none() {
                self.layers[i].routable = Some(true);
            }
            if self.layers[i].bend_cost.is_none() {
                self.layers[i].bend_cost = Some(default_bend_cost);
            }
            // The flip precedes the assignment; see the doc comment.
            if is_signal {
                curr_preferred_direction_is_horizontal = !curr_preferred_direction_is_horizontal;
            }
            if self.layers[i].preferred_direction_horizontal.is_none() {
                self.layers[i].preferred_direction_horizontal =
                    Some(curr_preferred_direction_is_horizontal);
            }

            if initialize_trace_costs {
                self.scoring.preferred_direction_trace_cost[i] = default_preferred;
                let add_costs = if curr_preferred_direction_is_horizontal {
                    horizontal_add_costs
                } else {
                    vertical_add_costs
                };
                self.scoring.undesired_direction_trace_cost[i] = default_undesired + add_costs;
            }
        }

        if initialize_trace_costs {
            let signal_layer_count = layer_structure.signal_layer_count();
            if signal_layer_count > 2 {
                let outer_add_costs = 0.2 * signal_layer_count as f64;
                let last = layer_count - 1;
                self.scoring.preferred_direction_trace_cost[0] += outer_add_costs;
                self.scoring.preferred_direction_trace_cost[last] += outer_add_costs;
                self.scoring.undesired_direction_trace_cost[0] += outer_add_costs;
                self.scoring.undesired_direction_trace_cost[last] += outer_add_costs;
            }
            self.board_specific_trace_costs_applied = true;
        }
    }

    /// `get_layer_active` (`RouterSettings.java:566-576`): out of range ⇒ `false`, unset ⇒ `true`.
    pub fn layer_active(&self, layer: usize) -> bool {
        match self.layers.get(layer) {
            None => false,
            Some(l) => l.routable.unwrap_or(true),
        }
    }

    /// `set_layer_active` (`RouterSettings.java:554-564`): out-of-range writes are dropped.
    pub fn set_layer_active(&mut self, layer: usize, value: bool) {
        if let Some(l) = self.layers.get_mut(layer) {
            l.routable = Some(value);
        }
    }

    /// `get_bend_cost` (`RouterSettings.java:590-600`): out of range ⇒ `0.0`; unset ⇒
    /// `scoring.default_bend_cost` clamped into `[MIN_BEND_COST, MAX_BEND_COST]`.
    ///
    /// Note the asymmetry upstream: the clamp is applied to the *default* fallback but an
    /// explicitly stored `bend_cost` is returned unclamped (it was clamped on set).
    pub fn bend_cost(&self, layer: usize) -> f64 {
        match self.layers.get(layer) {
            None => 0.0,
            Some(l) => match l.bend_cost {
                Some(v) => v,
                None => self
                    .scoring
                    .default_bend_cost
                    .map(|v| v.clamp(MIN_BEND_COST, MAX_BEND_COST))
                    .unwrap_or(0.0),
            },
        }
    }

    /// `set_bend_cost` (`RouterSettings.java:578-588`): clamped into
    /// `[MIN_BEND_COST, MAX_BEND_COST]`; out-of-range writes are dropped.
    pub fn set_bend_cost(&mut self, layer: usize, value: f64) {
        if let Some(l) = self.layers.get_mut(layer) {
            l.bend_cost = Some(value.clamp(MIN_BEND_COST, MAX_BEND_COST));
        }
    }

    /// `get_preferred_direction_is_horizontal` (`RouterSettings.java:609-618`): out of range ⇒
    /// `false`; unset ⇒ `layer % 2 == 1`.
    pub fn preferred_direction_is_horizontal(&self, layer: usize) -> bool {
        match self.layers.get(layer) {
            None => false,
            Some(l) => l.preferred_direction_horizontal.unwrap_or(layer % 2 == 1),
        }
    }

    /// `set_preferred_direction_is_horizontal` (`RouterSettings.java:597-607`).
    pub fn set_preferred_direction_is_horizontal(&mut self, layer: usize, value: bool) {
        if let Some(l) = self.layers.get_mut(layer) {
            l.preferred_direction_horizontal = Some(value);
        }
    }

    /// `get_preferred_direction_trace_costs` (`RouterSettings.java:635-645`): out of range ⇒ `0.0`
    /// (layer index beyond `layer_count`), unset array ⇒ `1.0`.
    pub fn preferred_direction_trace_costs(&self, layer: usize) -> f64 {
        if layer >= self.layer_count() {
            return 0.0;
        }
        self.scoring
            .preferred_direction_trace_cost
            .get(layer)
            .copied()
            .unwrap_or(1.0)
    }

    /// `set_preferred_direction_trace_costs` (`RouterSettings.java:620-633`): clamped to `>= 0.1`
    /// and marks the costs as user-owned so `apply_board_specific_optimizations` stops
    /// recomputing them.
    pub fn set_preferred_direction_trace_costs(&mut self, layer: usize, value: f64) {
        if layer >= self.layer_count() {
            return;
        }
        if self.scoring.preferred_direction_trace_cost.len() != self.layers.len() {
            self.scoring.preferred_direction_trace_cost = vec![0.0; self.layers.len()];
        }
        self.scoring.preferred_direction_trace_cost[layer] = value.max(0.1);
        self.board_specific_trace_costs_applied = true;
    }

    /// `get_against_preferred_direction_trace_costs` (`RouterSettings.java:647-657`).
    pub fn against_preferred_direction_trace_costs(&self, layer: usize) -> f64 {
        if layer >= self.layer_count() {
            return 0.0;
        }
        self.scoring
            .undesired_direction_trace_cost
            .get(layer)
            .copied()
            .unwrap_or(1.0)
    }

    /// `set_against_preferred_direction_trace_costs` (`RouterSettings.java:671-684`): clamped to
    /// `>= 0.1`, also marks the costs as user-owned.
    pub fn set_against_preferred_direction_trace_costs(&mut self, layer: usize, value: f64) {
        if layer >= self.layer_count() {
            return;
        }
        if self.scoring.undesired_direction_trace_cost.len() != self.layers.len() {
            self.scoring.undesired_direction_trace_cost = vec![0.0; self.layers.len()];
        }
        self.scoring.undesired_direction_trace_cost[layer] = value.max(0.1);
        self.board_specific_trace_costs_applied = true;
    }

    /// `get_horizontal_trace_costs` (`RouterSettings.java:659-669`): maps
    /// `(preferred, undesired)` onto the horizontal axis using the layer's preferred direction.
    pub fn horizontal_trace_costs(&self, layer: usize) -> f64 {
        if layer >= self.layer_count() {
            return 0.0;
        }
        if self.preferred_direction_is_horizontal(layer) {
            self.preferred_direction_trace_costs(layer)
        } else {
            self.against_preferred_direction_trace_costs(layer)
        }
    }

    /// `get_vertical_trace_costs` (`RouterSettings.java:686-696`).
    pub fn vertical_trace_costs(&self, layer: usize) -> f64 {
        if layer >= self.layer_count() {
            return 0.0;
        }
        if self.preferred_direction_is_horizontal(layer) {
            self.against_preferred_direction_trace_costs(layer)
        } else {
            self.preferred_direction_trace_costs(layer)
        }
    }

    /// `get_trace_cost_arr` (`RouterSettings.java:698-707`): materializes the per-layer
    /// `(horizontal, vertical)` pairs. Upstream builds a fresh array on every call, but calls it
    /// once per net setup, so this allocation is not on a hot path.
    pub fn trace_cost_arr(&self) -> Vec<ExpansionCostFactor> {
        (0..self.scoring.preferred_direction_trace_cost.len())
            .map(|i| ExpansionCostFactor {
                horizontal: self.horizontal_trace_costs(i),
                vertical: self.vertical_trace_costs(i),
            })
            .collect()
    }

    /// The only normalization step upstream performs (`RouterSettings.validate`,
    /// `RouterSettings.java:932-965`):
    ///
    /// * `max_passes` outside `0..=9999` ⇒ `9999`; exactly `0` ⇒ `i32::MAX` ("no limit").
    /// * `max_threads` capped at the available core count. Note it is *not* run through
    ///   [`normalize_max_threads`] here, so a `max_threads` of `0` survives `validate` unchanged —
    ///   only the setter maps `0` onto "all cores".
    /// * `trace_pull_tight_accuracy < 1` ⇒ `500`.
    pub fn validate(&mut self) {
        if self.max_passes < 0 || self.max_passes > MAX_PASSES_LIMIT {
            self.max_passes = MAX_PASSES_LIMIT;
        } else if self.max_passes == 0 {
            self.max_passes = i32::MAX;
        }

        self.max_threads = self.max_threads.min(available_processors());

        if self.trace_pull_tight_accuracy < 1 {
            self.trace_pull_tight_accuracy = DEFAULT_TRACE_PULL_TIGHT_ACCURACY;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_math_round_semantics() {
        assert_eq!(java_math_round(2.5), 3);
        // Java rounds halves towards +inf, unlike f64::round which rounds away from zero.
        assert_eq!(java_math_round(-2.5), -2);
        assert_eq!(java_math_round(2.4999), 2);
        // Java 7+ corner: floor(x + 0.5) would give 1 here.
        assert_eq!(java_math_round(0.499_999_999_999_999_94), 0);
        assert_eq!(java_math_round(f64::NAN), 0);
        assert_eq!(java_math_round(f64::INFINITY), i64::MAX);
        assert_eq!(java_math_round(f64::NEG_INFINITY), i64::MIN);
    }
}
