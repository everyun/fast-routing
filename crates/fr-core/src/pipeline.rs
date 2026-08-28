//! **The pipeline.** DSN bytes in, judged board and session file out.
//!
//! This is `RoutingJob` v2 — the M1 replacement for [`crate::legacy::routing_job`], and the
//! thing `fr-cli` actually runs. Four stages, each owned by the crate that specialises in it:
//!
//! ```text
//! bytes ──▶ fr_io::specctra::import ──▶ Board + BoardRules      [`RoutingJob::import`]
//!                                        │
//!                                        ▼
//!                              ┌── the router ──┐               [`RouterStage`]  ← M2 lands here
//!                              └───── (none) ───┘
//!                                        │
//!                                        ▼
//!                          fr_check: clearance + connectivity   [`Referee::judge`]
//!                          + the v2 score
//!                                        │
//!                                        ▼
//!                          fr_io::specctra::ses_write           [`write_session`]
//! ```
//!
//! # There is deliberately no router in it
//!
//! M1's contract is that *nothing routes*. The board that reaches the referee and the SES writer
//! is exactly the board the importer built, so a G1.1 or G1.2a delta can only ever be an
//! importer or a checker bug — "the router did it" is not available as an explanation. The old
//! `fr-autoroute` toy would happily run here and it must not: it is written against
//! [`fr_board::BasicBoard`], a second item model that the importer never produces, and its
//! output has never been judged by [`fr_check`]. It stays reachable through
//! [`crate::legacy`] and nowhere else.
//!
//! [`RouterStage`] is the seam. Today it has one variant; M2 adds the real one, and the only
//! code that changes is [`RoutingJob::run`]'s single `match`.
//!
//! # What "SES passthrough" means
//!
//! The session file carries the wiring the DSN already had — `(type route)` copper, plus
//! whatever a previous tool left behind — re-emitted through the v2 writer with its endpoint
//! snapping (`dsn-board-semantics.md` §12.3). It is a *valid* session that KiCad will accept;
//! it is not a *routed* one. Callers that need to tell the difference read
//! [`JobReport::routed`], and `fr-cli` turns that into its own exit code.
//!
//! ```no_run
//! let job = fr_core::RoutingJob::from_path("board.dsn").unwrap();
//! let report = job.run().unwrap();
//! println!("{} violations, {} incomplete, score {:.1}",
//!          report.stats.clearance_violation_count,
//!          report.stats.incomplete_count,
//!          report.score.normalized);
//! ```

use std::path::{Path, PathBuf};

use fr_board::item::ItemKind;
use fr_board::Board;
use fr_check::{
    board_connectivity, check_clearance, score, shape_store, BoardConnectivity, BoardStats, Score,
    ScoreWeights, StoreView, Violation,
};
use fr_io::specctra::{self, ImportError, ImportOptions, ImportedDesign, SesInput, Unit};

/// Where the router would go.
///
/// One variant today. M2a adds `Route(RouterSettings)` and the pipeline gains a stage between
/// the import and the referee; nothing else in this module moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RouterStage {
    /// Import, judge, write the imported wiring back out. No routing, no board mutation.
    #[default]
    ImportOnly,
}

/// Why a job could not produce a report.
#[derive(Debug)]
pub enum JobError {
    /// The design file could not be read.
    Read(PathBuf, std::io::Error),
    /// The importer rejected it.
    Import(ImportError),
}

impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JobError::Read(path, error) => write!(f, "read {}: {error}", path.display()),
            JobError::Import(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for JobError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            JobError::Read(_, error) => Some(error),
            JobError::Import(error) => Some(error),
        }
    }
}

/// A headless job: one design, one configuration, one report.
///
/// Deliberately neither `Clone` nor `#[derive(Debug)]`: `source` is the whole design file, tens
/// of megabytes on the larger fixtures, and both would copy or print it. Use
/// [`RoutingJob::import_with`] where a second configuration is needed.
pub struct RoutingJob {
    source: Vec<u8>,
    design_name: String,
    import_options: ImportOptions,
    weights: ScoreWeights,
    stage: RouterStage,
}

impl std::fmt::Debug for RoutingJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoutingJob")
            .field("design_name", &self.design_name)
            .field("source_bytes", &self.source.len())
            .field("import_options", &self.import_options)
            .field("weights", &self.weights)
            .field("stage", &self.stage)
            .finish()
    }
}

impl RoutingJob {
    /// Builds a job over DSN bytes.
    ///
    /// `design_name` is the `(base_design …)` token of the session file. Upstream passes the
    /// *file* name there, not the `(pcb <name>)` token, which is why it is a parameter and not
    /// read off the design.
    pub fn new(source: impl Into<Vec<u8>>, design_name: impl Into<String>) -> RoutingJob {
        RoutingJob {
            source: source.into(),
            design_name: design_name.into(),
            import_options: ImportOptions::default(),
            weights: ScoreWeights::V2,
            stage: RouterStage::default(),
        }
    }

    /// Builds a job from a path, taking the file name as the design name.
    pub fn from_path(path: impl AsRef<Path>) -> Result<RoutingJob, JobError> {
        let path = path.as_ref();
        let bytes =
            std::fs::read(path).map_err(|error| JobError::Read(path.to_path_buf(), error))?;
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok(RoutingJob::new(bytes, name))
    }

    /// The post-load overrides the import runs. Defaults to the shipped ones; `java -drc` runs
    /// them off (see [`crate::drc_report::DRC_IMPORT_OPTIONS`]).
    pub fn with_import_options(mut self, options: ImportOptions) -> RoutingJob {
        self.import_options = options;
        self
    }

    /// The scoring weights. Defaults to [`ScoreWeights::V2`], the 2.3.0 numbers.
    pub fn with_weights(mut self, weights: ScoreWeights) -> RoutingJob {
        self.weights = weights;
        self
    }

    /// Selects the router stage. Only [`RouterStage::ImportOnly`] exists in M1.
    pub fn with_stage(mut self, stage: RouterStage) -> RoutingJob {
        self.stage = stage;
        self
    }

    /// The `(base_design …)` token this job will write.
    pub fn design_name(&self) -> &str {
        &self.design_name
    }

    /// Stage 1 on its own: DSN bytes → board + rules.
    ///
    /// Exposed because the two gate emitters ([`fr_io::golden_dump`] and
    /// [`crate::drc_report`]) need the design and nothing downstream of it.
    pub fn import(&self) -> Result<ImportedDesign, JobError> {
        self.import_with(self.import_options)
    }

    /// [`RoutingJob::import`] with a one-off override of the post-load options.
    ///
    /// The `-drc` and `-do` oracles were generated under *different* override settings
    /// ([`crate::drc_report::DRC_IMPORT_OPTIONS`]), so a caller emitting both documents has to
    /// import twice. Taking the options here rather than through
    /// [`RoutingJob::with_import_options`] is what keeps that from cloning the whole design
    /// file, which on the larger fixtures is tens of megabytes.
    pub fn import_with(&self, options: ImportOptions) -> Result<ImportedDesign, JobError> {
        specctra::import_with_options(&self.source, options).map_err(JobError::Import)
    }

    /// The whole pipeline.
    pub fn run(&self) -> Result<JobReport, JobError> {
        let design = self.import()?;
        // The router stage. `ImportOnly` is a no-op *by construction*: it does not receive
        // `&mut design.board`, so no future edit to this match arm can start mutating the
        // board by accident.
        let routed = match self.stage {
            RouterStage::ImportOnly => false,
        };
        let referee = Referee::judge(&design, &self.weights);
        let session = write_session(&design, &self.design_name);
        Ok(JobReport {
            design_name: self.design_name.clone(),
            pcb_name: design.design_name.clone(),
            layer_count: design.board.layers().len(),
            census: BoardCensus::of(&design.board),
            stats: referee.stats,
            score: referee.score,
            violations: referee.violations,
            connectivity: referee.connectivity,
            ses: session.text,
            warnings: design
                .warnings
                .iter()
                .cloned()
                .chain(session.warnings)
                .collect(),
            routed,
            design,
        })
    }
}

/// Everything one job run produced.
pub struct JobReport {
    /// The `(base_design …)` token, i.e. the input file name.
    pub design_name: String,
    /// The `(pcb <name>)` token.
    pub pcb_name: String,
    /// Layers in the board stackup.
    pub layer_count: usize,
    /// Item counts by kind.
    pub census: BoardCensus,
    /// The five referee quantities the score is computed from.
    pub stats: BoardStats,
    /// The v2 score.
    pub score: Score,
    /// The deduplicated clearance violations.
    pub violations: Vec<Violation>,
    /// Per-net connectivity, incompletes and airlines.
    pub connectivity: BoardConnectivity,
    /// The session file text.
    pub ses: String,
    /// Importer and writer warnings, in that order.
    pub warnings: Vec<String>,
    /// **False in M1.** True only once a router stage actually changed the board; the SES is a
    /// passthrough of the imported wiring until then.
    pub routed: bool,
    /// The imported design, kept so a caller can re-read the session against it or emit a
    /// golden dump without importing twice.
    pub design: ImportedDesign,
}

impl JobReport {
    /// No violations and nothing left unconnected.
    ///
    /// On an M1 run this describes the *input* board, not our work: a fully pre-routed fixture
    /// is clean and an unrouted one is not, and neither says anything about a router that has
    /// not been written yet.
    pub fn is_clean(&self) -> bool {
        self.stats.clearance_violation_count == 0 && self.stats.incomplete_count == 0
    }
}

/// The referee's verdict on one board.
pub struct Referee {
    /// `check_clearance`, deduplicated per `(min_id, max_id, layer)`.
    pub violations: Vec<Violation>,
    /// `board_connectivity`.
    pub connectivity: BoardConnectivity,
    /// The score inputs, assembled from both plus the board's own geometry.
    pub stats: BoardStats,
    /// `score(stats, weights)`.
    pub score: Score,
}

impl Referee {
    /// Runs the clearance DRC and the ratsnest, then scores the result.
    pub fn judge(design: &ImportedDesign, weights: &ScoreWeights) -> Referee {
        let board = &design.board;
        let shapes = shape_store(board);
        let view = StoreView::new(
            board.store(),
            &shapes,
            &design.rules.clearance_matrix,
            board.layers().len(),
        );
        let violations = check_clearance(&view);
        let connectivity = board_connectivity(&view);
        let mut stats = board_geometry_stats(design);
        stats.max_connections = connectivity.max_connections as u64;
        stats.incomplete_count = connectivity.incomplete_count as u64;
        stats.clearance_violation_count = violations.len() as u64;
        let score = score(&stats, weights);
        Referee {
            violations,
            connectivity,
            stats,
            score,
        }
    }
}

/// The three geometry-derived score inputs: bends, trace length, vias.
///
/// `BoardStatistics.java:150-336`, transcribed:
///
/// - **bends** = `Σ over traces with cornerCount >= 3 of (cornerCount - 2)` (`:252-268`). A
///   two-corner trace is straight and contributes nothing.
/// - **trace length** = `Σ PolylineTrace.get_length()` over *every* trace regardless of fixed
///   state (`:171-176`), converted with `Unit.scale(1, boardUnit, MM) / max(resolution, 1)`
///   (`:194-203`).
/// - **vias** = `board.get_vias().size()` (`:308`).
///
/// # Two things that look like bugs and are the contract
///
/// 1. The unit conversion divides by the DSN **resolution**, not by the board **scale factor**.
///    They differ exactly when `Structure.createBoard`'s integer-division reduction fired
///    (`transform.rs`, §3.2), and on such a board upstream's millimetres are off by the
///    reduction ratio. Reproduced, because the score is a comparison target.
/// 2. `plane_via_count` is left at zero and every via is charged at `via_costs`. Upstream's
///    score line is `this.vias.totalCount * scoringSettings.viaCosts` (`:566`) — `planeViaCosts`
///    is a *router* cost, never a *scoring* one (`batch-orchestration.md` §6.1, row 76).
fn board_geometry_stats(design: &ImportedDesign) -> BoardStats {
    // `Unit.scale(1.0, unit, UM) / max(resolution, 1)` — micrometres per board unit.
    let resolution = f64::from(design.resolution.max(1));
    let board_unit_to_um = Unit::scale(1.0, design.unit, Unit::Um) / resolution;

    let mut bend_count = 0u64;
    let mut trace_length_board = 0.0f64;
    let mut via_count = 0u64;
    for (_, item) in design.board.store().iter() {
        match &item.kind {
            ItemKind::Trace(trace) => {
                let corners = trace.corner_count();
                if corners >= 3 {
                    bend_count += (corners - 2) as u64;
                }
                trace_length_board += trace.polyline.length_total();
            }
            ItemKind::Via(_) => via_count += 1,
            _ => {}
        }
    }

    BoardStats {
        bend_count,
        // `f64 -> u64` saturates at 0 for negatives and at u64::MAX for overflow, neither of
        // which a length can reach; a NaN would become 0, and a NaN length is a geometry bug
        // that the board consistency check catches first.
        trace_length_um: (trace_length_board * board_unit_to_um).round() as u64,
        via_count,
        plane_via_count: 0,
        ..BoardStats::default()
    }
}

/// Item counts by kind — the cheap summary a CLI prints and a test asserts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BoardCensus {
    /// Component pads.
    pub pins: usize,
    /// Vias.
    pub vias: usize,
    /// Traces.
    pub traces: usize,
    /// Keepouts of all three flavours.
    pub obstacle_areas: usize,
    /// Copper pours and power planes.
    pub conduction_areas: usize,
    /// Board outline items.
    pub board_outlines: usize,
    /// Component metadata geometry.
    pub component_outlines: usize,
}

impl BoardCensus {
    /// Counts every item in the store.
    pub fn of(board: &Board) -> BoardCensus {
        let mut census = BoardCensus::default();
        for (_, item) in board.store().iter() {
            match &item.kind {
                ItemKind::Pin(_) => census.pins += 1,
                ItemKind::Via(_) => census.vias += 1,
                ItemKind::Trace(_) => census.traces += 1,
                ItemKind::ObstacleArea(_) => census.obstacle_areas += 1,
                ItemKind::ConductionArea(_) => census.conduction_areas += 1,
                ItemKind::BoardOutline(_) => census.board_outlines += 1,
                ItemKind::ComponentOutline(_) => census.component_outlines += 1,
            }
        }
        census
    }

    /// Every item, of every kind.
    pub fn total(&self) -> usize {
        self.pins
            + self.vias
            + self.traces
            + self.obstacle_areas
            + self.conduction_areas
            + self.board_outlines
            + self.component_outlines
    }
}

/// Stage 4 on its own: the v2 session writer, with endpoint snapping.
///
/// Separate from [`RoutingJob::run`] so that a caller holding a board a router has just edited
/// can write it without rebuilding the job.
pub fn write_session(design: &ImportedDesign, design_name: &str) -> specctra::SesOutput {
    specctra::write_session_verbose(&SesInput::from_design(design, design_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two pins on one net, one straight `(type route)` wire between them, on a 10 mm board.
    ///
    /// Every number the assertions below use is derived from this text: `(resolution um 10)`
    /// with a 10 000-unit outline keeps the scale factor at 10 (`transform.rs`), so a board unit
    /// is 0.1 µm and the wire's 5000 DSN µm are 50 000 board units.
    const BOARD: &str = r#"(pcb pipeline_smoke
  (parser (string_quote "))
  (resolution um 10)
  (unit um)
  (structure
    (layer F.Cu (type signal))
    (layer B.Cu (type signal))
    (boundary (path signal 0 0 0 10000 0 10000 10000 0 10000 0 0))
    (rule (width 250) (clearance 200))
  )
  (placement
    (component pad2
      (place P1 1000 1000 front 0)
      (place P2 6000 1000 front 0)
    )
  )
  (library
    (image pad2 (pin round_pad 1 0 0))
    (padstack round_pad (shape (circle F.Cu 400)) (shape (circle B.Cu 400)) (attach off))
  )
  (network
    (net SIG (pins P1-1 P2-1))
    (class default SIG (rule (width 250) (clearance 200)))
  )
  (wiring
    (wire (path F.Cu 250 1000 1000 6000 1000) (net SIG) (type route))
  )
)"#;

    fn job() -> RoutingJob {
        RoutingJob::new(BOARD.as_bytes(), "pipeline_smoke.dsn")
    }

    #[test]
    fn the_pipeline_imports_judges_and_writes() {
        let report = job().run().expect("the board imports");
        assert_eq!(report.pcb_name, "pipeline_smoke");
        assert_eq!(report.layer_count, 2);
        assert_eq!(report.census.pins, 2);
        assert_eq!(report.census.traces, 1);
        // The session is the v2 writer's, not the pre-rewrite one's: `(base_design …)` carries
        // the *file* name we passed, and the session name is that with `.dsn` -> `.ses`. Both
        // are quoted, because `_` is in `SES_RESERVED_CHARS` (v2 widened the set so KiCad net
        // names survive a round trip).
        assert!(
            report.ses.starts_with("(session \"pipeline_smoke.ses\""),
            "{}",
            &report.ses[..report.ses.len().min(60)]
        );
        assert!(report
            .ses
            .contains("(base_design \"pipeline_smoke.dsn\")"));
        assert!(report.ses.contains("(routes"));
    }

    #[test]
    fn nothing_is_routed_and_the_report_says_so() {
        let report = job().run().expect("the board imports");
        assert!(!report.routed, "M1 must not route");
        // The one wire in the fixture is the one wire in the session: a passthrough, not a route.
        assert_eq!(report.census.traces, 1);
    }

    /// The wire spans 5000 µm between the two pad centres, so the net is complete and the board
    /// is clean — which makes the score the maximum, 1000.
    #[test]
    fn a_complete_two_pin_net_scores_a_thousand() {
        let report = job().run().expect("the board imports");
        assert_eq!(report.stats.max_connections, 1);
        assert_eq!(report.stats.incomplete_count, 0);
        assert_eq!(report.stats.clearance_violation_count, 0);
        assert!(report.is_clean());
        // maximum 5e6, penalties 0, costs = 5000 µm * 1.0/mm + 0 vias = 5.0.
        assert_eq!(report.score.maximum, 5_000_000.0);
        assert_eq!(report.score.penalties, 0.0);
        assert!(
            (report.score.normalized - 999.999).abs() < 0.01,
            "{}",
            report.score.normalized
        );
    }

    /// A straight two-corner wire has no bends, and its length converts through the DSN
    /// resolution: 50 000 board units * (1 µm / 10) = 5000 µm.
    #[test]
    fn geometry_stats_are_converted_through_the_resolution() {
        let design = job().import().expect("the board imports");
        let stats = board_geometry_stats(&design);
        assert_eq!(stats.bend_count, 0);
        assert_eq!(stats.trace_length_um, 5_000);
        assert_eq!(stats.via_count, 0);
        assert_eq!(stats.plane_via_count, 0);
    }

    #[test]
    fn a_non_dsn_input_is_a_typed_error_not_a_panic() {
        let job = RoutingJob::new(b"not a board".to_vec(), "junk.dsn");
        match job.run() {
            Err(JobError::Import(ImportError::NotADsnFile)) => {}
            Err(other) => panic!("wrong error: {other}"),
            Ok(_) => panic!("junk must not import"),
        }
    }

    #[test]
    fn a_missing_file_is_a_read_error() {
        match RoutingJob::from_path("/nonexistent/board.dsn") {
            Err(JobError::Read(path, _)) => assert!(path.ends_with("board.dsn")),
            _ => panic!("a missing file must be a read error"),
        }
    }
}
