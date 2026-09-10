//! Engine observer and routing event contracts.
//!
//! Defined per `docs/tasks/GUI-CONTRACT.md`.

use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use fr_board::Board;

use crate::snapshot::{RenderModel, Stats};

/// Lifecycle and progress events emitted by the router engine.
#[derive(Debug, Clone)]
pub enum RouterEvent {
    /// Fired once after design import and before routing begins.
    JobStarted {
        /// Base design name.
        design: String,
        /// Number of board layers.
        layers: u16,
        /// Total number of connections to route.
        connections: u32,
    },
    /// Fired after each fanout pass completes.
    FanoutPass {
        /// 1-based fanout pass number.
        pass: u32,
        /// Pins escaped in this pass.
        routed: u32,
        /// Total pins escaped across all fanout passes so far.
        escapes: u32,
        /// Elapsed time of this fanout pass in seconds.
        elapsed_s: f64,
    },
    /// Fired at the beginning of each autoroute pass.
    PassStarted {
        /// 1-based autoroute pass number.
        pass: u32,
        /// Incomplete connections at the start of this pass.
        incomplete: u32,
    },
    /// Fired after attempting to route a single connection.
    ConnectionDone {
        /// 1-based pass number.
        pass: u32,
        /// Net number of the connection.
        net: i32,
        /// Whether the connection was successfully routed.
        ok: bool,
        /// Number of conflicting items ripped up.
        ripped: u32,
        /// Time spent on this connection in milliseconds.
        elapsed_ms: f64,
    },
    /// Fired at the end of each autoroute pass after scoring.
    PassFinished {
        /// 1-based autoroute pass number.
        pass: u32,
        /// Incomplete connections at the end of this pass.
        incomplete: u32,
        /// Clearance violations at the end of this pass.
        violations: u32,
        /// Wall time of this pass in seconds.
        elapsed_s: f64,
        /// Normalized score at the end of this pass.
        score: f64,
    },
    /// Throttled snapshot of the current board state and stats.
    Snapshot(Arc<RenderModel>),
    /// Fired when routing is stopped early (e.g. deadline or cancellation).
    Stopped {
        /// Stop reason label (e.g. `"cancelled"`, `"deadline"`, `"interrupted"`).
        reason: String,
    },
    /// Fired when the whole job finishes normally.
    Finished {
        /// Final incomplete count.
        incomplete: u32,
        /// Final clearance violation count.
        violations: u32,
        /// Total job elapsed wall time in seconds.
        elapsed_s: f64,
    },
}

impl RouterEvent {
    /// Formats this event as a single JSON line.
    ///
    /// Per GUI contract: [`RouterEvent::Snapshot`] outputs only its stats summary.
    pub fn to_json_line(&self) -> String {
        match self {
            RouterEvent::JobStarted {
                design,
                layers,
                connections,
            } => {
                let escaped_design = design.replace('\\', "\\\\").replace('"', "\\\"");
                format!(
                    r#"{{"event":"JobStarted","design":"{}","layers":{},"connections":{}}}"#,
                    escaped_design, layers, connections
                )
            }
            RouterEvent::FanoutPass {
                pass,
                routed,
                escapes,
                elapsed_s,
            } => {
                format!(
                    r#"{{"event":"FanoutPass","pass":{},"routed":{},"escapes":{},"elapsed_s":{:.6}}}"#,
                    pass, routed, escapes, elapsed_s
                )
            }
            RouterEvent::PassStarted { pass, incomplete } => {
                format!(
                    r#"{{"event":"PassStarted","pass":{},"incomplete":{}}}"#,
                    pass, incomplete
                )
            }
            RouterEvent::ConnectionDone {
                pass,
                net,
                ok,
                ripped,
                elapsed_ms,
            } => {
                format!(
                    r#"{{"event":"ConnectionDone","pass":{},"net":{},"ok":{},"ripped":{},"elapsed_ms":{:.3}}}"#,
                    pass, net, ok, ripped, elapsed_ms
                )
            }
            RouterEvent::PassFinished {
                pass,
                incomplete,
                violations,
                elapsed_s,
                score,
            } => {
                format!(
                    r#"{{"event":"PassFinished","pass":{},"incomplete":{},"violations":{},"elapsed_s":{:.6},"score":{:.4}}}"#,
                    pass, incomplete, violations, elapsed_s, score
                )
            }
            RouterEvent::Snapshot(model) => {
                let s = &model.stats;
                format!(
                    r#"{{"event":"Snapshot","stats":{{"incomplete":{},"violations":{},"pass":{},"elapsed_s":{:.6},"score":{:.4},"items":{}}}}}"#,
                    s.incomplete, s.violations, s.pass, s.elapsed_s, s.score, s.items
                )
            }
            RouterEvent::Stopped { reason } => {
                let escaped_reason = reason.replace('\\', "\\\\").replace('"', "\\\"");
                format!(r#"{{"event":"Stopped","reason":"{}"}}"#, escaped_reason)
            }
            RouterEvent::Finished {
                incomplete,
                violations,
                elapsed_s,
            } => {
                format!(
                    r#"{{"event":"Finished","incomplete":{},"violations":{},"elapsed_s":{:.6}}}"#,
                    incomplete, violations, elapsed_s
                )
            }
        }
    }
}

/// Observer interface for receiving routing engine events.
pub trait RouterObserver: Send {
    /// Called synchronously when an event occurs.
    fn on_event(&mut self, ev: RouterEvent);

    /// Whether the observer wants full snapshot events.
    fn wants_snapshot(&self) -> bool {
        true
    }
}

impl RouterObserver for Box<dyn RouterObserver> {
    fn on_event(&mut self, ev: RouterEvent) {
        (**self).on_event(ev);
    }

    fn wants_snapshot(&self) -> bool {
        (**self).wants_snapshot()
    }
}

impl RouterObserver for std::sync::mpsc::Sender<RouterEvent> {
    fn on_event(&mut self, ev: RouterEvent) {
        let _ = self.send(ev);
    }
}

impl<F: FnMut(RouterEvent) + Send> RouterObserver for F {
    fn on_event(&mut self, ev: RouterEvent) {
        self(ev);
    }
}

/// A snapshot generator callback: `(&Board, Stats) -> Arc<RenderModel>`.
pub type SnapshotFn = Box<dyn Fn(&Board, Stats) -> Arc<RenderModel> + Send>;

/// Configuration for engine observation, throttling, and cancellation.
pub struct RouterConfig {
    /// Optional event observer.
    pub observer: Option<Box<dyn RouterObserver>>,
    /// Minimum milliseconds between in-pass snapshot captures.
    pub snapshot_interval_ms: u64,
    /// Optional snapshot model producer.
    pub snapshot_fn: Option<SnapshotFn>,
    /// Cooperative cancellation flag checked at deadline checkpoints.
    pub cancel: Arc<AtomicBool>,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            observer: None,
            snapshot_interval_ms: 500,
            snapshot_fn: None,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl RouterConfig {
    /// Builds a new default router configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Attaches an observer.
    pub fn with_observer(mut self, observer: Box<dyn RouterObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Sets the minimum throttling interval between in-pass snapshots in milliseconds.
    pub fn with_snapshot_interval_ms(mut self, interval_ms: u64) -> Self {
        self.snapshot_interval_ms = interval_ms;
        self
    }

    /// Sets the snapshot producer function.
    pub fn with_snapshot_fn(mut self, f: SnapshotFn) -> Self {
        self.snapshot_fn = Some(f);
        self
    }

    /// Sets the shared cancellation flag.
    pub fn with_cancel(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = cancel;
        self
    }
}

/// An observer that records each event as a JSON line into a file.
pub struct FileTraceObserver {
    writer: std::io::BufWriter<std::fs::File>,
}

impl FileTraceObserver {
    /// Opens or creates the target file at `path` for writing JSON lines.
    pub fn new(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let file = std::fs::File::create(path)?;
        Ok(Self {
            writer: std::io::BufWriter::new(file),
        })
    }
}

impl RouterObserver for FileTraceObserver {
    fn on_event(&mut self, ev: RouterEvent) {
        let line = ev.to_json_line();
        let _ = writeln!(self.writer, "{line}");
        let _ = self.writer.flush();
    }
}
