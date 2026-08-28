//! The pre-rewrite pipeline, kept alive only until M2 replaces the router.
//!
//! This is the `DsnDocument` → [`fr_board::BasicBoard`] → `fr_autoroute::BatchAutorouter` →
//! `fr_drc::DesignRulesChecker` → [`fr_io::SesWriter`] chain that `fr-cli` used to run. Every
//! crate it touches is on the same list: [`fr_io::dsn_reader`] (a non-conformant tokenizer),
//! [`fr_board::legacy`] (a parallel item model), `fr-autoroute` and `fr-drc`.
//!
//! It is **not** the product any more — [`crate::pipeline`] is, and `fr-cli` drives that. This
//! module exists for exactly two reasons:
//!
//! 1. `fr-autoroute` still compiles against [`fr_board::legacy`], so deleting the legacy board
//!    now would take the router with it. M2 rewrites the router against
//!    [`fr_board::Board`]; that is when this module and `fr-drc` go.
//! 2. The fixture tests written against it are the only surviving record of what the toy router
//!    could do, and losing them silently would make the M2 comparison unfalsifiable.
//!
//! # It is behind the `legacy` cargo feature
//!
//! Default-on, so `cargo test --workspace` still runs those tests rather than skipping them —
//! a feature-gated test that nobody runs is worse than no test. `--no-default-features` builds
//! the new stack alone, which is the configuration M2 will make the only one:
//!
//! ```sh
//! cargo build -p fr-core --no-default-features   # pipeline v2 only; no fr-autoroute, no fr-drc
//! ```
//!
//! Nothing outside this module may call into it. In particular [`crate::pipeline`] never
//! constructs a [`fr_board::BasicBoard`]: the M1 contract is that the new stack is the only
//! thing that ever touches an imported board, so that "the router did it" can never be an
//! explanation for a G1.1/G1.2a delta.

pub mod board_statistics;
pub mod routing_job;

pub use board_statistics::BoardStatistics;
pub use routing_job::{JobResult, RoutingJob};
