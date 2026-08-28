//! The improvement lattice — `autoroute/ItemRouteResult.java`, transcribed.
//!
//! One re-routed item is judged by a **lexicographic** comparison of three
//! numbers measured before and after the reroute:
//!
//! ```text
//! fewer incompletes  ≻  fewer vias  ≻  shorter trace length  ≻  (tie = not improved)
//! ```
//!
//! Ties at every level are *not* an improvement, which is what makes the
//! optimizer monotone: an item is kept only when the board strictly moved down
//! the lattice, so `(incompletes, vias, length)` can never rise.
//!
//! # The unit mismatch is upstream's, and it is reproduced
//!
//! `batch-orchestration.md` conformance row **#87**: the release builds the
//! result with `trace_length_before = min_cumulative_trace_length` — a
//! **weighted** length, seeded from `BoardStatistics.traces.totalWeightedLength`
//! (`autoroute/BatchOptimizer.java:225`) — and
//! `trace_length_after = boardStatisticsAfter.traces.totalLength`, the **raw,
//! unweighted** length (`:390-393`). The two are incommensurable; on a typical
//! board `weighted ≫ raw` because the weight is `half_width + clearance` per
//! unit of length, so the "shorter trace length" branch fires almost
//! unconditionally once incompletes and vias tie.
//!
//! [`LengthMetric::UpstreamMixed`] is that behaviour and is the default, because
//! this crate's contract is release parity. [`LengthMetric::WeightedBoth`] is
//! the fix the appendix recommends ("port the intent, not the code") and is what
//! makes "improved ⇒ the board really got shorter" a provable statement; it is
//! opt-in so that turning it on is a visible, reviewable decision.

/// Which length pair [`ItemRouteResult`] compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LengthMetric {
    /// Release parity: weighted `before` against **unweighted** `after`
    /// (`BatchOptimizer.java:225, 390-393`; conformance #87). The default.
    #[default]
    UpstreamMixed,
    /// Weighted against weighted — the appendix's recommended fix. Under this
    /// metric "improved" implies the board's `(incompletes, vias, weighted
    /// length)` key strictly decreased.
    WeightedBoth,
}

/// The three numbers the lattice orders, measured on one board.
///
/// `BoardStatistics.items.viaCount` + `traces.totalLength` +
/// `traces.totalWeightedLength` + the DRC incomplete count, i.e. everything
/// `BatchOptimizer.opt_route_item` reads (`:315-321`, `:381-388`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BoardKey {
    /// `RouterCounters.incompleteCount` — the ratsnest count.
    pub incomplete_count: usize,
    /// `BoardStatistics.items.viaCount`.
    pub via_count: usize,
    /// `BoardStatistics.traces.totalLength`, raw board units.
    pub trace_length: f64,
    /// `BoardStatistics.traces.totalWeightedLength`, `length * (half_width +
    /// clearance)` and halved for shove-fixed traces.
    pub weighted_trace_length: f64,
}

/// The verdict on one re-routed item — `ItemRouteResult`.
///
/// The field names are upstream's `*_before` / `*_after`, and
/// [`ItemRouteResult::improved`] is computed in the constructor exactly as
/// `ItemRouteResult.java:29-47` does it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ItemRouteResult {
    /// Incompletes before the ripout.
    pub incomplete_count_before: usize,
    /// Incompletes after the reroute.
    pub incomplete_count_after: usize,
    /// Vias before the ripout.
    pub via_count_before: usize,
    /// Vias after the reroute.
    pub via_count_after: usize,
    /// The `before` side of the length comparison. Under
    /// [`LengthMetric::UpstreamMixed`] this is the optimizer's running
    /// `min_cumulative_trace_length`, which is a **weighted** length.
    pub trace_length_before: f64,
    /// The `after` side. Under [`LengthMetric::UpstreamMixed`] this is the raw
    /// unweighted board length; under [`LengthMetric::WeightedBoth`] it is the
    /// weighted one.
    pub trace_length_after: f64,
    improved: bool,
}

impl ItemRouteResult {
    /// The `ItemRouteResult(int p_item_id)` one-argument constructor
    /// (`ItemRouteResult.java:16-19`): the "we did not even try" result, with
    /// `incomplete_count_after = 1 > 0 = before` so that it can never look like
    /// an improvement.
    pub fn not_attempted() -> ItemRouteResult {
        ItemRouteResult {
            incomplete_count_before: 0,
            incomplete_count_after: 1,
            via_count_before: 0,
            via_count_after: 0,
            trace_length_before: 0.0,
            trace_length_after: 0.0,
            improved: false,
        }
    }

    /// The seven-argument constructor (`ItemRouteResult.java:20-51`).
    ///
    /// `min_cumulative_trace_length` is the optimizer's running weighted-length
    /// floor, which upstream passes as `trace_length_before` regardless of what
    /// `after` is measured in — see the module docs and conformance #87.
    pub fn new(
        before: &BoardKey,
        after: &BoardKey,
        min_cumulative_trace_length: f64,
        metric: LengthMetric,
    ) -> ItemRouteResult {
        // `BatchOptimizer.java:381-388`. `trace_length_before` is the running
        // weighted minimum in **both** metrics — that part is not the bug; the
        // bug (conformance #87) is that the release measures `after` with
        // `traces.totalLength`, the unweighted sum.
        let trace_length_after = match metric {
            LengthMetric::UpstreamMixed => after.trace_length,
            LengthMetric::WeightedBoth => after.weighted_trace_length,
        };
        let mut result = ItemRouteResult {
            incomplete_count_before: before.incomplete_count,
            incomplete_count_after: after.incomplete_count,
            via_count_before: before.via_count,
            via_count_after: after.via_count,
            trace_length_before: min_cumulative_trace_length,
            trace_length_after,
            improved: false,
        };
        result.improved = result.compute_improved();
        result
    }

    /// `ItemRouteResult.java:29-47`, branch for branch. Every tie is `false`.
    fn compute_improved(&self) -> bool {
        if self.incomplete_count_after < self.incomplete_count_before {
            return true;
        }
        if self.incomplete_count_after > self.incomplete_count_before {
            return false;
        }
        if self.via_count_after < self.via_count_before {
            return true;
        }
        if self.via_count_after > self.via_count_before {
            return false;
        }
        // The one f64 comparison in the lattice. It decides whether a *reroute
        // is kept*, never a piece of geometry, and both sides are sums the
        // caller measured on the same board in the same units, so an exact
        // `<` is the whole rule (`ItemRouteResult.java:39-45`).
        self.trace_length_after < self.trace_length_before
    }

    /// `improved()`.
    pub fn improved(&self) -> bool {
        self.improved
    }

    /// `update_improved(boolean)` — `opt_route_item` ANDs the user-stop flag
    /// into the verdict before acting on it (`BatchOptimizer.java:394-395`).
    pub fn update_improved(&mut self, improved: bool) {
        self.improved = improved;
    }

    /// `via_count()` — the *after* count, which is what `opt_route_pass` feeds
    /// into its ratio.
    pub fn via_count(&self) -> usize {
        self.via_count_after
    }

    /// `trace_length()` — likewise the *after* length.
    pub fn trace_length(&self) -> f64 {
        self.trace_length_after
    }

    /// `incomplete_count()`.
    pub fn incomplete_count(&self) -> usize {
        self.incomplete_count_after
    }

    /// `compareTo`, as an ordering on the *after* triple. Used by nothing on the
    /// single-threaded path — `improved_over` is the multi-threaded optimizer's
    /// candidate picker, which the release only reaches from the GUI
    /// (conformance #100) — but it is the same lattice and it is cheap to keep
    /// honest.
    pub fn improved_over(&self, other: &ItemRouteResult) -> bool {
        if self.incomplete_count_after != other.incomplete_count_after {
            return self.incomplete_count_after < other.incomplete_count_after;
        }
        if self.via_count_after != other.via_count_after {
            return self.via_count_after < other.via_count_after;
        }
        self.trace_length_after < other.trace_length_after
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(incomplete: usize, vias: usize, length: f64) -> BoardKey {
        BoardKey {
            incomplete_count: incomplete,
            via_count: vias,
            trace_length: length,
            weighted_trace_length: length,
        }
    }

    fn verdict(before: BoardKey, after: BoardKey, floor: f64) -> bool {
        ItemRouteResult::new(&before, &after, floor, LengthMetric::WeightedBoth).improved()
    }

    #[test]
    fn fewer_incompletes_wins_over_more_vias_and_more_length() {
        assert!(verdict(key(2, 1, 100.0), key(1, 9, 900.0), 100.0));
    }

    #[test]
    fn more_incompletes_loses_however_short_the_result() {
        assert!(!verdict(key(1, 9, 900.0), key(2, 0, 1.0), 900.0));
    }

    #[test]
    fn vias_break_an_incomplete_tie_before_length_does() {
        assert!(verdict(key(1, 3, 100.0), key(1, 2, 999.0), 100.0));
        assert!(!verdict(key(1, 2, 999.0), key(1, 3, 1.0), 999.0));
    }

    #[test]
    fn length_breaks_a_via_tie() {
        assert!(verdict(key(0, 2, 100.0), key(0, 2, 99.0), 100.0));
        assert!(!verdict(key(0, 2, 100.0), key(0, 2, 101.0), 100.0));
    }

    #[test]
    fn every_tie_is_not_improved() {
        assert!(!verdict(key(0, 2, 100.0), key(0, 2, 100.0), 100.0));
        assert!(!verdict(key(3, 0, 0.0), key(3, 0, 0.0), 0.0));
    }

    #[test]
    fn the_length_floor_and_not_the_before_length_is_compared() {
        // `before.trace_length` is deliberately ignored: upstream passes the
        // running `min_cumulative_trace_length` as `trace_length_before`
        // (`BatchOptimizer.java:383`), so a pass that already found something
        // shorter refuses to accept a regression back to the start length.
        let before = key(0, 1, 500.0);
        let after = key(0, 1, 450.0);
        assert!(!verdict(before, after, 400.0), "the floor is 400, not 500");
        assert!(verdict(before, after, 460.0));
    }

    #[test]
    fn the_upstream_metric_compares_weighted_before_with_unweighted_after() {
        // Conformance #87: this reroute made the board *longer* in weighted
        // terms, and upstream still calls it an improvement because the raw
        // length it compares against the weighted floor is far smaller.
        let before = BoardKey {
            incomplete_count: 0,
            via_count: 1,
            trace_length: 1_000.0,
            weighted_trace_length: 70_000.0,
        };
        let after = BoardKey {
            incomplete_count: 0,
            via_count: 1,
            trace_length: 1_200.0,
            weighted_trace_length: 84_000.0,
        };
        let floor = before.weighted_trace_length;
        assert!(
            ItemRouteResult::new(&before, &after, floor, LengthMetric::UpstreamMixed).improved()
        );
        assert!(
            !ItemRouteResult::new(&before, &after, floor, LengthMetric::WeightedBoth).improved(),
            "the fixed metric sees the regression the release misses"
        );
    }

    #[test]
    fn the_not_attempted_result_never_looks_like_an_improvement() {
        assert!(!ItemRouteResult::not_attempted().improved());
    }

    #[test]
    fn improved_over_orders_the_after_triple() {
        let cheap = ItemRouteResult::new(
            &key(0, 5, 100.0),
            &key(0, 1, 100.0),
            100.0,
            LengthMetric::WeightedBoth,
        );
        let dear = ItemRouteResult::new(
            &key(0, 5, 100.0),
            &key(0, 4, 100.0),
            100.0,
            LengthMetric::WeightedBoth,
        );
        assert!(cheap.improved_over(&dear));
        assert!(!dear.improved_over(&cheap));
        assert!(!cheap.improved_over(&cheap));
    }
}
