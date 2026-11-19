//! Background routing bridge between GPUI frontend and the core routing engine.
//!
//! Defined per `docs/tasks/GUI-CONTRACT.md` and `docs/tasks/TASK-gui-integ.md`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use fr_core::snapshot::{RatLine, RenderModel, Stats};
use fr_core::{MazeOptions, RouterEvent, RouterStage, RoutingJob};

/// User-configurable routing job parameters from the GUI settings panel.
#[derive(Debug, Clone)]
pub struct JobParams {
    /// Specctra DSN file path.
    pub dsn_path: PathBuf,
    /// Ripup-and-reroute passes (-mp).
    pub max_passes: i32,
    /// Total job timeout in seconds (None = no timeout).
    pub job_timeout_secs: Option<u64>,
    /// Tiling 45 search mode toggle.
    pub tiling45_mode: bool,
    /// Run SMD pin-escape pre-pass (fanout).
    pub fanout: bool,
    /// Run optimizer stage after autorouting.
    pub optimize: bool,
    /// Effort-based virtual time budget clock (--deterministic).
    pub deterministic: bool,
    /// Worker thread count for optimizer & referee.
    pub threads: Option<usize>,
    /// Parallel connection workers budget.
    pub parallel_connections: Option<usize>,
    /// Initial ripup start costs override.
    pub ripup_costs: Option<i32>,
    /// Minimum throttling interval between in-pass snapshots in milliseconds.
    pub snapshot_interval_ms: u64,
}

impl Default for JobParams {
    fn default() -> Self {
        Self {
            dsn_path: PathBuf::new(),
            max_passes: 1,
            job_timeout_secs: None,
            tiling45_mode: false,
            fanout: true,
            optimize: true,
            deterministic: false,
            threads: None,
            parallel_connections: None,
            ripup_costs: None,
            snapshot_interval_ms: 500,
        }
    }
}

/// Status of the background routing process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Idle,
    Loading,
    Running,
    Paused,
    Stopping,
    Stopped,
    Finished,
    Failed,
}

impl JobState {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobState::Idle => "Idle",
            JobState::Loading => "Loading",
            JobState::Running => "Routing",
            JobState::Paused => "Paused",
            JobState::Stopping => "Stopping",
            JobState::Stopped => "Stopped",
            JobState::Finished => "Finished",
            JobState::Failed => "Failed",
        }
    }
}

/// A log entry recorded by the engine bridge.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub timestamp_s: f64,
    pub message: String,
    pub is_warning: bool,
}

/// The bridge controller managing background routing execution and UI state synchronization.
pub struct JobBridge {
    pub state: JobState,
    pub params: JobParams,
    pub current_model: Arc<RenderModel>,
    pub stats: Stats,
    pub logs: Vec<LogEntry>,
    pub incompletes: Vec<RatLine>,
    pub selected_incomplete: Option<usize>,
    pub last_ses_text: Option<String>,
    pub current_design_name: String,

    // Threading & IPC
    event_rx: Option<Receiver<RouterEvent>>,
    cancel_flag: Arc<AtomicBool>,
    pause_flag: Arc<AtomicBool>,
    worker_thread: Option<JoinHandle<Result<String, String>>>,
    start_instant: Option<Instant>,
}

impl Default for JobBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl JobBridge {
    /// Creates a new inactive bridge with default parameters and an empty render model.
    pub fn new() -> Self {
        Self {
            state: JobState::Idle,
            params: JobParams::default(),
            current_model: Arc::new(RenderModel {
                layers: Vec::new(),
                bbox: [0.0; 4],
                resolution: 0.0001,
                traces: Vec::new(),
                vias: Vec::new(),
                pads: Vec::new(),
                areas: Vec::new(),
                ratsnest: Vec::new(),
                stats: Stats::default(),
            }),
            stats: Stats::default(),
            logs: vec![LogEntry {
                timestamp_s: 0.0,
                message: "Fast-Routing GUI Bridge initialized.".into(),
                is_warning: false,
            }],
            incompletes: Vec::new(),
            selected_incomplete: None,
            last_ses_text: None,
            current_design_name: String::new(),
            event_rx: None,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            pause_flag: Arc::new(AtomicBool::new(false)),
            worker_thread: None,
            start_instant: None,
        }
    }

    /// Loads a DSN file immediately without starting the router, taking an initial snapshot.
    pub fn open_dsn(&mut self, path: impl AsRef<Path>) -> Result<(), String> {
        let path = path.as_ref();
        let job = RoutingJob::from_path(path).map_err(|e| format!("Failed to read DSN: {e:?}"))?;
        let design = job.import().map_err(|e| format!("Import failed: {e:?}"))?;

        let initial_stats = Stats {
            incomplete: 0,
            violations: 0,
            pass: 0,
            elapsed_s: 0.0,
            score: 0.0,
            items: design.board.store().len() as u32,
        };
        let model = fr_core::snapshot::snapshot(&design.board, initial_stats);
        let incomplete_count = model.ratsnest.len();

        self.params.dsn_path = path.to_path_buf();
        self.current_design_name = design.design_name.clone();
        self.incompletes = model.ratsnest.clone();
        self.stats = Stats {
            incomplete: incomplete_count as u32,
            ..model.stats
        };
        self.current_model = Arc::new(model);
        self.state = JobState::Idle;
        self.last_ses_text = None;
        self.selected_incomplete = None;

        self.log(format!(
            "Loaded design '{}': {} layers, {} items, {} incompletes.",
            path.file_name().unwrap_or_default().to_string_lossy(),
            self.current_model.layers.len(),
            self.stats.items,
            self.stats.incomplete,
        ));
        Ok(())
    }

    /// Starts or restarts the routing job in a background thread.
    pub fn start(&mut self) -> Result<(), String> {
        if self.params.dsn_path.as_os_str().is_empty() {
            return Err("No DSN file opened. Please open a .dsn design file first.".into());
        }
        if self.state == JobState::Running {
            return Ok(());
        }

        // Reset cancellation and pause flags
        self.cancel_flag.store(false, Ordering::SeqCst);
        self.pause_flag.store(false, Ordering::SeqCst);

        let (event_tx, event_rx) = mpsc::channel::<RouterEvent>();
        self.event_rx = Some(event_rx);
        self.state = JobState::Running;
        let start_time = Instant::now();
        self.start_instant = Some(start_time);

        let params = self.params.clone();
        let cancel_flag = self.cancel_flag.clone();

        self.log(format!(
            "Starting router job: max_passes={}, fanout={}, det={}, parallel_conn={:?}, interval={}ms",
            params.max_passes,
            params.fanout,
            params.deterministic,
            params.parallel_connections,
            params.snapshot_interval_ms
        ));

        let handle = thread::spawn(move || -> Result<String, String> {
            let mut job = RoutingJob::from_path(&params.dsn_path)
                .map_err(|e| format!("Failed to read DSN: {e:?}"))?;

            let maze_opts = MazeOptions::with_passes(params.max_passes)
                .with_fanout(params.fanout)
                .with_deterministic(params.deterministic)
                .with_optimizer(params.optimize)
                .with_threads(params.threads)
                .with_parallel_connections(params.parallel_connections);

            if let Some(timeout) = params.job_timeout_secs {
                fr_core::arm_job_deadline(Duration::from_secs(timeout));
            }

            job = job
                .with_stage(RouterStage::Maze(maze_opts))
                .with_observer(Box::new(event_tx))
                .with_snapshot_interval_ms(params.snapshot_interval_ms)
                .with_cancel(cancel_flag);

            let report = job.run().map_err(|e| format!("Routing failed: {e:?}"))?;
            Ok(report.ses)
        });

        self.worker_thread = Some(handle);
        Ok(())
    }

    /// Stops the routing job cooperatively via cancellation flag.
    pub fn stop(&mut self) {
        if self.state == JobState::Running || self.state == JobState::Paused {
            self.cancel_flag.store(true, Ordering::SeqCst);
            self.state = JobState::Stopping;
            self.log("Cancel signal sent to routing engine...".into());
        }
    }

    /// Pauses routing (if engine supports pause or holds at pass boundary).
    pub fn pause(&mut self) {
        if self.state == JobState::Running {
            self.pause_flag.store(true, Ordering::SeqCst);
            self.state = JobState::Paused;
            self.log("Routing paused by user.".into());
        }
    }

    /// Resumes routing from paused state.
    pub fn resume(&mut self) {
        if self.state == JobState::Paused {
            self.pause_flag.store(false, Ordering::SeqCst);
            self.state = JobState::Running;
            self.log("Routing resumed.".into());
        }
    }

    /// Saves the current or completed session (.ses) file.
    pub fn save_ses(&mut self, output_path: impl AsRef<Path>) -> Result<(), String> {
        let path = output_path.as_ref();
        if let Some(ses_text) = &self.last_ses_text {
            std::fs::write(path, ses_text).map_err(|e| format!("Failed to write SES: {e}"))?;
            self.log(format!("Saved SES session file to '{}'", path.display()));
            return Ok(());
        }

        // If job has not produced SES yet, generate one from the current board if loaded
        if !self.params.dsn_path.as_os_str().is_empty() {
            let job = RoutingJob::from_path(&self.params.dsn_path)
                .map_err(|e| format!("Failed to load design: {e:?}"))?
                .with_stage(RouterStage::ImportOnly)
                .with_session_path(path);
            let _report = job
                .run()
                .map_err(|e| format!("Failed to export SES: {e:?}"))?;
            self.log(format!(
                "Generated and saved SES session file to '{}'",
                path.display()
            ));
            return Ok(());
        }

        Err("No design or session data available to save.".into())
    }

    /// Polls and consumes incoming routing events from the background thread.
    ///
    /// Should be called on every frame or periodic UI update.
    /// Returns `true` if a new snapshot was received and the canvas needs a repaint.
    pub fn poll_events(&mut self) -> bool {
        let mut model_updated = false;

        let mut events = Vec::new();
        if let Some(rx) = &self.event_rx {
            loop {
                match rx.try_recv() {
                    Ok(ev) => events.push(ev),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => break,
                }
            }
        }
        for ev in events {
            if self.handle_router_event(ev) {
                model_updated = true;
            }
        }

        // Check worker thread termination
        if let Some(handle) = self.worker_thread.take() {
            if handle.is_finished() {
                match handle.join() {
                    Ok(Ok(ses)) => {
                        self.last_ses_text = Some(ses);
                        if self.state == JobState::Stopping || self.state == JobState::Stopped {
                            self.state = JobState::Stopped;
                        } else {
                            self.state = JobState::Finished;
                        }
                        self.log("Routing worker thread finished successfully.".into());
                    }
                    Ok(Err(err)) => {
                        self.state = JobState::Failed;
                        self.log_warning(format!("Routing worker failed: {err}"));
                    }
                    Err(panic_err) => {
                        self.state = JobState::Failed;
                        self.log_warning(format!("Routing worker thread panicked: {panic_err:?}"));
                    }
                }
            } else {
                self.worker_thread = Some(handle);
            }
        }

        model_updated
    }

    /// Handles a single [`RouterEvent`]. Returns true if a snapshot was received.
    fn handle_router_event(&mut self, ev: RouterEvent) -> bool {
        match ev {
            RouterEvent::JobStarted {
                design,
                layers,
                connections,
            } => {
                self.log(format!(
                    "Engine started: design='{}', layers={}, connections={}",
                    design, layers, connections
                ));
                self.stats.incomplete = connections;
                false
            }
            RouterEvent::FanoutPass {
                pass,
                routed,
                escapes,
                elapsed_s,
            } => {
                self.log(format!(
                    "Fanout pass {}: routed {}, total escaped {}, in {:.2}s",
                    pass, routed, escapes, elapsed_s
                ));
                false
            }
            RouterEvent::PassStarted { pass, incomplete } => {
                self.log(format!(
                    "Pass {} started: {} connections to route",
                    pass, incomplete
                ));
                self.stats.pass = pass;
                self.stats.incomplete = incomplete;
                false
            }
            RouterEvent::ConnectionDone {
                pass: _,
                net: _,
                ok: _,
                ripped: _,
                elapsed_ms: _,
            } => {
                // Throttle connection logs to avoid flooding UI event queue
                false
            }
            RouterEvent::PassFinished {
                pass,
                incomplete,
                violations,
                elapsed_s,
                score,
            } => {
                self.stats.pass = pass;
                self.stats.incomplete = incomplete;
                self.stats.violations = violations;
                self.stats.elapsed_s = elapsed_s;
                self.stats.score = score;
                self.log(format!(
                    "Pass {} finished: {} incomplete, {} violations, score {:.2}, in {:.2}s",
                    pass, incomplete, violations, score, elapsed_s
                ));
                false
            }
            RouterEvent::Snapshot(model) => {
                self.stats = model.stats;
                self.incompletes = model.ratsnest.clone();
                self.current_model = model;
                true
            }
            RouterEvent::Stopped { reason } => {
                self.state = JobState::Stopped;
                self.log(format!("Routing stopped early: {}", reason));
                false
            }
            RouterEvent::Finished {
                incomplete,
                violations,
                elapsed_s,
            } => {
                self.stats.incomplete = incomplete;
                self.stats.violations = violations;
                self.stats.elapsed_s = elapsed_s;
                self.log(format!(
                    "Routing finished: {} incomplete, {} violations, total {:.2}s",
                    incomplete, violations, elapsed_s
                ));
                false
            }
        }
    }

    /// Appends an informational log message.
    pub fn log(&mut self, message: String) {
        let elapsed = self
            .start_instant
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        self.logs.push(LogEntry {
            timestamp_s: elapsed,
            message,
            is_warning: false,
        });
        if self.logs.len() > 1000 {
            self.logs.drain(0..200);
        }
    }

    /// Appends a warning log message.
    pub fn log_warning(&mut self, message: String) {
        let elapsed = self
            .start_instant
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        self.logs.push(LogEntry {
            timestamp_s: elapsed,
            message,
            is_warning: true,
        });
    }
}
