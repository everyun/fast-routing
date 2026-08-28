//! **Deprecated — superseded by `fr-check`. Scheduled for deletion with M2.**
//!
//! # Why it is still in the workspace
//!
//! The M1 retirement audit found exactly one remaining caller: `fr_core::legacy::routing_job`,
//! the pre-rewrite pipeline. That module cannot drop this dependency, because it is written
//! against `fr_board::BasicBoard` and `fr-check` only speaks
//! `fr_board::Board` — porting it would mean porting the toy
//! router, which is what M2 does by replacing it. So the crate stays, and it is now **optional**
//! behind `fr-core`'s default-on `legacy` feature:
//!
//! ```sh
//! cargo tree -p fr-cli --no-default-features   # fr-drc is not in the graph
//! ```
//!
//! Removing it from `Cargo.toml`'s `members` today would not have removed it from the build:
//! Cargo makes every path dependency of a member a member too, so the only real removal is
//! deleting the directory, and that has to wait for its last caller.
//!
//! # Why `fr-check` supersedes it rather than merely duplicating it
//!
//! | | `fr-drc` | `fr-check` |
//! |---|---|---|
//! | item pairs examined | trace × trace only — pins, vias, keepouts, pours and the board outline are invisible to it | every connectable and every obstacle |
//! | geometry | distance between *corner points*, minus the two half widths; a segment interior is never sampled | the real convex tile shapes from the search tree |
//! | metric | Euclidean | the octagon Minkowski norm the jar uses, integer-rounded (`drc-connectivity.md` §1.5) |
//! | verdict | `distance < clearance` | `dimension(enlarged ∩ enlarged) == 2` — touching is legal |
//! | clearances | one `f64` the caller passes in | the real `ClearanceMatrix`, per class pair and per layer |
//! | incompletes | "≥ 2 pins and no traces ⇒ every consecutive pin pair" | connected sets over normal contacts, then an MST over the node set |
//! | oracle | none | gate G1.2a, against `java -drc` on 45 boards |
//!
//! The two therefore disagree on real boards by construction, and only `fr-check`'s number may
//! be quoted as "the M in `(N unrouted and M violations)`". Nothing new may call this crate.
//!
//! Mirrors `app.freerouting.drc` from the upstream Java project:
//!
//! - `DesignRulesChecker::get_all_clearance_violations` — clearance checker.
//! - `ClearanceViolation` — structured clearance violation info.
//! - `NetIncompletes`, `AirLine` — unrouted net / ratsnest analysis.

pub mod clearance_violation;
pub mod drc_checker;
pub mod net_incompletes;

pub use clearance_violation::ClearanceViolation;
pub use drc_checker::DesignRulesChecker;
pub use net_incompletes::{AirLine, NetIncompletes};
