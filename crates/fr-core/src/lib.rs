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
//! # M1 status: the pipeline does not route
//!
//! [`pipeline::RouterStage`] has one variant, `ImportOnly`. The board that reaches the referee
//! and the session writer is the board the importer built, unmodified, so every number this
//! crate reports is a property of the *input design* and of `fr-io` / `fr-check`. That is
//! deliberate — it is what makes G1.1 and G1.2a measure the importer and the checker rather
//! than a router. M2 adds the routing stage, and takes [`legacy`] and `fr-drc` out with it.

pub mod drc_report;
#[cfg(feature = "legacy")]
pub mod legacy;
pub mod pipeline;

pub use drc_report::{report as drc_json_report, ReportOptions, DRC_IMPORT_OPTIONS};
pub use pipeline::{BoardCensus, JobError, JobReport, Referee, RouterStage, RoutingJob};

/// Returns the crate version.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
