//! Headless orchestration for the Freerouting Rust port: the pipeline, and the gate emitters.
//!
//! `app.freerouting.core` upstream. Three public things:
//!
//! | module | role |
//! |---|---|
//! | [`pipeline`] | **`RoutingJob` v2** — DSN → [`fr_board::Board`] → [`fr_check`] referee → SES. What `fr-cli` runs. |
//! | [`drc_report`] | gate G1.2a's `java -drc`-shaped JSON emitter |
//! | [`legacy`] | the pre-rewrite `DsnDocument` → `BasicBoard` → `fr-autoroute` chain, behind the default-on `legacy` feature, deleted by M2 |
//!
//! The golden-dump emitter gate G1.1 reads is not here: it is [`fr_io::golden_dump::emit`],
//! which takes an [`fr_io::specctra::ImportedDesign`] and needs nothing this crate owns.
//! `fr-cli --golden-dump` calls it directly.
//!
//! # Routing is opt-in
//!
//! [`pipeline::RouterStage::ImportOnly`] is still the default, and under it the board that
//! reaches the referee and the session writer is the board the importer built, unmodified — so
//! every number a `--import-only`, `--golden-dump` or `--drc-report` run reports is a property
//! of the *input design* and of `fr-io` / `fr-check`. That is what keeps G1.1 and G1.2a
//! measuring the importer and the checker rather than a router.
//!
//! [`pipeline::RouterStage::Maze`] is the M2a router: [`fr_router`]'s fanout stage (opt-in) and
//! batch pass driver, writing into the imported board in place. [`legacy`] and `fr-drc` go out
//! when M2 completes.

pub mod drc_report;
#[cfg(feature = "legacy")]
pub mod legacy;
pub mod pipeline;

pub use drc_report::{report as drc_json_report, ReportOptions, DRC_IMPORT_OPTIONS};
pub use pipeline::{
    arm_job_deadline, job_budget_spent, request_job_stop, BoardCensus, JobError, JobReport,
    MazeOptions, OptimizerReport, Referee, RouterReport, RouterStage, RoutingJob,
};

/// Returns the crate version.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
