//! The per-connection budget — `ARCHITECTURE.md` "Budgets (policy reversed
//! after review)", implemented over upstream's constant.
//!
//! Upstream gives every connection of pass *n* a wall-clock allowance of
//! `min(100_000 · 2^(n−1), Integer.MAX_VALUE)` milliseconds
//! (`autoroute/BatchAutorouter.java:1311-1313`, conformance appendix row 46):
//! 100 s on pass 1, doubling per pass, saturating at ~24.9 days on pass 16. The
//! `TimeLimit` it builds from that goes into `board.init_autoroute(...)` and is
//! read inside the maze loop.
//!
//! ARCHITECTURE fixes the policy this port applies to it:
//!
//! > Default mode keeps **upstream's wall-clock budget constants verbatim** …
//! > they define the reference's stopping behavior, and on a faster core they
//! > simply bind less, which is the honest way to convert speed into quality.
//! > An **effort ceiling** (expansion pops, shove visits, pull-tight iterations)
//! > exists as a secondary cap only, default unbounded; bench records
//! > `time_used`/`effort_used` and `connections_hitting_budget` per board so
//! > budget sensitivity is a number, not a vibe. `--deterministic` mode replaces
//! > wall checks with effort-based virtual time … so byte-reproducibility holds.
//!
//! So there are exactly three knobs, and this module is all three:
//!
//! | knob | type | default |
//! |---|---|---|
//! | upstream wall constant | [`upstream_connection_budget_ms`] | primary |
//! | effort ceiling | [`BudgetPolicy::effort_ceiling`] | `None` = unbounded |
//! | virtual time | [`BudgetMode::Deterministic`] | off |
//!
//! # Where it binds
//!
//! [`ConnectionClock`] is armed by the batch pass around every connection
//! ([`BatchRouter::autoroute_pass_with`](crate::batch::BatchRouter::autoroute_pass_with))
//! and travels down to the [`AutorouteEngine`](crate::engine::AutorouteEngine),
//! which is where upstream's `TimeLimit` lives (`AutorouteEngine.java:63`,
//! `:119`). Two things then happen inside the maze:
//!
//! * every settled element charges one unit ([`ConnectionClock::charge`]) —
//!   the maze's own unit of work, and the currency
//!   [`BudgetMode::Deterministic`] buys virtual milliseconds with;
//! * the top of `occupy_next_element`'s pop loop polls
//!   `AutorouteEngine::is_stop_requested`, exactly where upstream polls it
//!   (`MazeSearchAlgo.java:258`), and leaves with
//!   [`SearchFailure::Stopped`](crate::maze::search::SearchFailure::Stopped) —
//!   which the pass reports as `NOT_ROUTED`, upstream's outcome for the same
//!   event.
//!
//! The allowance is also **accounted** for: [`ConnectionUsage::time_used_ms`],
//! [`ConnectionUsage::effort_used`] and [`ConnectionUsage::hit_budget`] are
//! summed into [`PassOutcome`](crate::batch::PassOutcome), which is where the
//! bench numbers ARCHITECTURE asks for are read from. The clock is armed around
//! the *whole* attempt — search, insert and the per-connection pull-tight — and
//! only the search polls it, which is upstream's arrangement too.
//!
//! Because the shipped constant is 100 s per connection and no connection in the
//! fixture suite comes close, the enforcement never fires on a healthy board;
//! what it buys is that a pathological one is stopped and *reported* rather than
//! silently costing an hour.
//!
//! # Determinism
//!
//! In [`BudgetMode::Deterministic`] the wall clock is never read at all —
//! [`ConnectionClock`] holds no [`Instant`] to read — and the allowance is spent in
//! *virtual* milliseconds bought with effort at
//! [`BudgetPolicy::effort_per_virtual_ms`]. That is what makes the mode
//! byte-reproducible (gate G2.2): no decision can depend on machine load,
//! because nothing asks the machine anything.

use std::time::{Duration, Instant};

/// Upstream's base allowance, in milliseconds: pass 1 gets 100 s
/// (`BatchAutorouter.java:1311`).
pub const UPSTREAM_BASE_CONNECTION_BUDGET_MS: u64 = 100_000;

/// Upstream clamps the doubled allowance to `Integer.MAX_VALUE` milliseconds
/// before casting it to the `TimeLimit`'s `int` (`:1312`).
pub const UPSTREAM_CONNECTION_BUDGET_CEILING_MS: u64 = i32::MAX as u64;

/// Effort units that buy one virtual millisecond in
/// [`BudgetMode::Deterministic`].
///
/// **Provisional.** ARCHITECTURE requires this to be "calibrated ≥ upstream
/// effort via lab-jar counters", i.e. the M0.5 expansion-counter campaign sets
/// the real number. Until it does, the value is deliberately generous: at the
/// default 100 s pass-1 allowance it buys `2 × 10^9` effort units per
/// connection, which no connection can reach, so the deterministic budget never
/// binds *earlier* than upstream's wall clock would. Erring in that direction
/// keeps `--deterministic` a debugging mode that changes timing, not results.
pub const DEFAULT_EFFORT_PER_VIRTUAL_MS: u64 = 20_000;

/// `min(100_000 · 2^(pass_no − 1), i32::MAX)` milliseconds — upstream's
/// per-connection allowance for pass `pass_no` (1-based).
///
/// `pass_no <= 1` is the base allowance; upstream computes the exponent in
/// `double` and clamps, so passes past 16 all get the ceiling.
pub fn upstream_connection_budget_ms(pass_no: i32) -> u64 {
    scaled_budget_ms(UPSTREAM_BASE_CONNECTION_BUDGET_MS, pass_no)
}

fn scaled_budget_ms(base_ms: u64, pass_no: i32) -> u64 {
    let exponent = u32::try_from(pass_no.max(1) - 1).unwrap_or(0);
    if exponent >= 64 {
        return UPSTREAM_CONNECTION_BUDGET_CEILING_MS;
    }
    let scaled = (base_ms as u128) << exponent;
    scaled.min(UPSTREAM_CONNECTION_BUDGET_CEILING_MS as u128) as u64
}

/// Which clock the budget is spent against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BudgetMode {
    /// Upstream's wall clock. The default, per ARCHITECTURE "Budgets".
    #[default]
    Wall,
    /// Effort-based virtual time; the wall clock is never read. `--deterministic`.
    Deterministic,
}

/// The three budget knobs, as one value the batch loop carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetPolicy {
    /// Wall or virtual time.
    pub mode: BudgetMode,
    /// Pass-1 allowance in milliseconds; doubles per pass.
    pub base_connection_ms: u64,
    /// Secondary cap in effort units, `None` = unbounded (the default).
    pub effort_ceiling: Option<u64>,
    /// Effort units per virtual millisecond in [`BudgetMode::Deterministic`].
    pub effort_per_virtual_ms: u64,
}

impl Default for BudgetPolicy {
    /// Upstream's constants, no effort ceiling.
    fn default() -> Self {
        BudgetPolicy {
            mode: BudgetMode::Wall,
            base_connection_ms: UPSTREAM_BASE_CONNECTION_BUDGET_MS,
            effort_ceiling: None,
            effort_per_virtual_ms: DEFAULT_EFFORT_PER_VIRTUAL_MS,
        }
    }
}

impl BudgetPolicy {
    /// The `--deterministic` policy: same allowance, spent in virtual time.
    pub fn deterministic() -> BudgetPolicy {
        BudgetPolicy {
            mode: BudgetMode::Deterministic,
            ..BudgetPolicy::default()
        }
    }

    /// True when deterministic virtual budget mode is active.
    #[inline]
    pub fn is_deterministic(&self) -> bool {
        matches!(self.mode, BudgetMode::Deterministic)
    }

    /// Adds the secondary effort cap.
    pub fn with_effort_ceiling(mut self, ceiling: Option<u64>) -> BudgetPolicy {
        self.effort_ceiling = ceiling;
        self
    }

    /// The budget one connection of `pass_no` gets.
    pub fn for_pass(&self, pass_no: i32) -> ConnectionBudget {
        ConnectionBudget {
            mode: self.mode,
            limit_ms: scaled_budget_ms(self.base_connection_ms, pass_no),
            effort_ceiling: self.effort_ceiling,
            effort_per_virtual_ms: self.effort_per_virtual_ms.max(1),
        }
    }
}

/// One connection's allowance — upstream's `TimeLimit`, plus the effort cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionBudget {
    mode: BudgetMode,
    limit_ms: u64,
    effort_ceiling: Option<u64>,
    effort_per_virtual_ms: u64,
}

impl Default for ConnectionBudget {
    /// Pass 1 under the default policy.
    fn default() -> Self {
        BudgetPolicy::default().for_pass(1)
    }
}

impl ConnectionBudget {
    /// The allowance in milliseconds, wall or virtual.
    pub fn limit_ms(&self) -> u64 {
        self.limit_ms
    }

    /// The wall-clock allowance, or `None` in [`BudgetMode::Deterministic`]
    /// where no wall clock is ever consulted.
    pub fn wall_limit(&self) -> Option<Duration> {
        match self.mode {
            BudgetMode::Wall => Some(Duration::from_millis(self.limit_ms)),
            BudgetMode::Deterministic => None,
        }
    }

    /// The secondary effort cap, if one was configured.
    pub fn effort_ceiling(&self) -> Option<u64> {
        self.effort_ceiling
    }

    /// Which clock this budget is spent against.
    pub fn mode(&self) -> BudgetMode {
        self.mode
    }

    /// Arms the budget for one connection.
    pub fn start(&self) -> ConnectionClock {
        ConnectionClock {
            budget: *self,
            started: match self.mode {
                BudgetMode::Wall => Some(Instant::now()),
                BudgetMode::Deterministic => None,
            },
            effort: 0,
        }
    }
}

/// The armed budget of one in-flight connection.
#[derive(Debug)]
pub struct ConnectionClock {
    budget: ConnectionBudget,
    /// `None` in [`BudgetMode::Deterministic`]: the mode's whole point is that
    /// nothing reads the wall clock.
    started: Option<Instant>,
    effort: u64,
}

impl ConnectionClock {
    /// Charges `units` of effort — expansion pops, shove visits, pull-tight
    /// iterations. The hook the maze search consumes once it takes a budget.
    #[inline]
    pub fn charge(&mut self, units: u64) {
        self.effort = self.effort.saturating_add(units);
    }

    /// Effort charged so far.
    #[inline]
    pub fn effort(&self) -> u64 {
        self.effort
    }

    /// Which clock this connection is spending — what a stage inside the
    /// connection's window needs to pick its own guard's currency, e.g.
    /// [`crate::pull_tight::StallGuard`].
    #[inline]
    pub fn mode(&self) -> BudgetMode {
        self.budget.mode()
    }

    /// Whether the allowance is gone — upstream's `TimeLimit.limit_exceeded()`,
    /// plus the effort ceiling.
    ///
    /// Written to be cheap enough for an expansion loop: in
    /// [`BudgetMode::Deterministic`] it is pure arithmetic on the effort
    /// counter, and in [`BudgetMode::Wall`] it is one `Instant::now`, which is
    /// exactly what upstream pays.
    pub fn is_exhausted(&self) -> bool {
        if self
            .budget
            .effort_ceiling
            .is_some_and(|ceiling| self.effort >= ceiling)
        {
            return true;
        }
        self.elapsed_ms() >= self.budget.limit_ms
    }

    /// Milliseconds spent, wall or virtual.
    fn elapsed_ms(&self) -> u64 {
        match self.started {
            Some(started) => started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            None => self.effort / self.budget.effort_per_virtual_ms,
        }
    }

    /// Closes the connection out and reports what it cost.
    pub fn finish(self) -> ConnectionUsage {
        ConnectionUsage {
            time_used_ms: self.elapsed_ms(),
            effort_used: self.effort,
            hit_budget: self.is_exhausted(),
        }
    }
}

/// What one connection cost — the per-board bench numbers ARCHITECTURE asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConnectionUsage {
    /// Milliseconds spent, wall or virtual.
    pub time_used_ms: u64,
    /// Effort units charged.
    pub effort_used: u64,
    /// Whether the connection reached its allowance.
    pub hit_budget: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_allowance_doubles_per_pass_from_a_hundred_seconds() {
        assert_eq!(upstream_connection_budget_ms(1), 100_000);
        assert_eq!(upstream_connection_budget_ms(2), 200_000);
        assert_eq!(upstream_connection_budget_ms(3), 400_000);
        assert_eq!(upstream_connection_budget_ms(8), 100_000 * 128);
    }

    /// `:1312` — `Math.min(max_milliseconds, Integer.MAX_VALUE)`. Pass 16 is the
    /// first one whose doubled allowance exceeds the ceiling.
    #[test]
    fn the_allowance_saturates_at_integer_max_value() {
        assert_eq!(upstream_connection_budget_ms(15), 100_000 * 16_384);
        assert_eq!(
            upstream_connection_budget_ms(16),
            UPSTREAM_CONNECTION_BUDGET_CEILING_MS
        );
        assert_eq!(
            upstream_connection_budget_ms(9_999),
            UPSTREAM_CONNECTION_BUDGET_CEILING_MS
        );
    }

    #[test]
    fn a_pass_number_below_one_gets_the_base_allowance() {
        assert_eq!(upstream_connection_budget_ms(0), 100_000);
        assert_eq!(upstream_connection_budget_ms(-5), 100_000);
    }

    #[test]
    fn deterministic_mode_never_reads_the_wall_clock() {
        let budget = BudgetPolicy::deterministic().for_pass(1);
        assert_eq!(budget.wall_limit(), None);
        let clock = budget.start();
        assert!(clock.started.is_none());
        // No effort charged, so no virtual time has passed no matter how long
        // the process sits here.
        assert!(!clock.is_exhausted());
        assert_eq!(clock.finish().time_used_ms, 0);
    }

    #[test]
    fn virtual_time_is_bought_with_effort() {
        let policy = BudgetPolicy {
            base_connection_ms: 10,
            effort_per_virtual_ms: 100,
            ..BudgetPolicy::deterministic()
        };
        let mut clock = policy.for_pass(1).start();
        clock.charge(999);
        assert!(!clock.is_exhausted(), "9 virtual ms of 10");
        clock.charge(1);
        assert!(clock.is_exhausted(), "1000 effort = 10 virtual ms");
        let usage = clock.finish();
        assert_eq!(usage.time_used_ms, 10);
        assert_eq!(usage.effort_used, 1_000);
        assert!(usage.hit_budget);
    }

    #[test]
    fn the_effort_ceiling_is_a_secondary_cap_and_is_off_by_default() {
        assert_eq!(BudgetPolicy::default().effort_ceiling, None);
        let policy = BudgetPolicy::default().with_effort_ceiling(Some(4));
        let mut clock = policy.for_pass(1).start();
        clock.charge(3);
        assert!(!clock.is_exhausted());
        clock.charge(1);
        assert!(clock.is_exhausted(), "the ceiling binds before 100 s does");
    }
}
