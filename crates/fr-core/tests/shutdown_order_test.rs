//! **The session is written before the board is judged.**
//!
//! This is the ordering a run that gets killed depends on. `bench/run_bench.py` caps every run
//! and `SIGKILL`s the process group at the cap; `Issue209-split10` was killed there with no
//! `.ses` on disk, and under the old order — referee, then session — everything the run had
//! done was lost with it. Under this order the same kill costs a *report* about a board the
//! caller already has.
//!
//! A kill cannot be scheduled precisely enough to test directly, so the phases are observed
//! instead ([`fr_core::RoutingJob::run_observed`]): the assertions below stand in a SIGTERM's
//! place at each phase boundary and ask what a caller would have found on disk if the process
//! had died right there. `fr-cli`'s `a_sigterm_does_not_cost_us_the_session` covers the real
//! signal end to end.
//!
//! Hermetic: one synthetic two-net board in a temporary directory, no fixtures.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use fr_core::{JobPhase, MazeOptions, RouterStage, RoutingJob};

/// Four pads on two nets, nothing wired: the router has real work and the writer has real
/// copper to emit. Same board as `fr-cli`'s `UNROUTED_BOARD`, kept here so this test binary
/// needs nothing outside itself.
const BOARD: &str = r#"(pcb shutdown_order
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
    (component pad4
      (place P1 1000 1000 front 0)
      (place P2 6000 1000 front 0)
      (place P3 1000 4000 front 0)
      (place P4 6000 4000 front 0)
    )
  )
  (library
    (image pad4 (pin round_pad 1 0 0))
    (padstack round_pad (shape (circle F.Cu 400)) (shape (circle B.Cu 400)) (attach off))
  )
  (network
    (net SIG (pins P1-1 P2-1))
    (net SIG2 (pins P3-1 P4-1))
    (class default SIG SIG2 (rule (width 250) (clearance 200)))
  )
)"#;

/// A scratch directory, removed when the test ends.
struct Workspace {
    dir: PathBuf,
}

impl Workspace {
    fn new(tag: &str) -> Workspace {
        let dir = std::env::temp_dir().join(format!("fr-core-order-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the scratch directory");
        Workspace { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// Everything in the directory, sorted — used to prove no temporary file survives.
    fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.dir)
            .expect("the directory is readable")
            .map(|entry| entry.expect("an entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn job() -> RoutingJob {
    RoutingJob::new(BOARD.as_bytes().to_vec(), "board.dsn")
        .with_stage(RouterStage::Maze(MazeOptions::with_passes(1)))
}

#[test]
fn the_session_is_on_disk_before_the_referee_runs() {
    let workspace = Workspace::new("order");
    let ses = workspace.path("out.ses");

    // What a `SIGTERM` landing at each phase boundary would have left behind.
    let seen: RefCell<Vec<(JobPhase, bool)>> = RefCell::new(Vec::new());
    let report = job()
        .with_session_path(&ses)
        .run_observed(&mut |phase| seen.borrow_mut().push((phase, ses.is_file())))
        .expect("the job runs");

    let seen = seen.into_inner();
    let phases: Vec<JobPhase> = seen.iter().map(|(phase, _)| *phase).collect();
    assert_eq!(
        phases,
        vec![
            JobPhase::Imported,
            JobPhase::Routed,
            JobPhase::SessionWritten,
            JobPhase::Judged,
        ],
        "the pipeline's phase order is the contract"
    );
    let on_disk: Vec<bool> = seen.iter().map(|(_, exists)| *exists).collect();
    assert_eq!(
        on_disk,
        vec![false, false, true, true],
        "the file must appear at SessionWritten — i.e. before the referee, not after it"
    );

    // And it is the real session, not a placeholder the referee would have finished.
    let written = std::fs::read_to_string(&ses).expect("the session was written");
    assert_eq!(written, report.ses, "the file and the report carry one text");
    assert!(written.starts_with("(session board.ses"), "{written}");
    assert!(written.contains("(wire"), "the routed copper is in it: {written}");
    assert_eq!(report.session_path.as_deref(), Some(ses.as_path()));
}

/// The write is atomic, so a kill in the middle of it cannot leave a *shorter* session that
/// still parses — the one corruption a harness cannot detect. What is observable from outside
/// is that the temporary the rename comes from does not survive the run.
#[test]
fn the_write_leaves_no_temporary_behind() {
    let workspace = Workspace::new("atomic");
    let ses = workspace.path("out.ses");
    job()
        .with_session_path(&ses)
        .run()
        .expect("the job runs");
    assert_eq!(workspace.entries(), vec!["out.ses".to_string()]);

    // A second run over the same path replaces it rather than appending to it.
    let report = job()
        .with_session_path(&ses)
        .run()
        .expect("the second job runs");
    assert_eq!(workspace.entries(), vec!["out.ses".to_string()]);
    assert_eq!(
        std::fs::read_to_string(&ses).expect("still there"),
        report.ses
    );
}

/// A job with no session path writes nothing at all: the caller owns `report.ses` and its own
/// IO, exactly as before. This is what the gate emitters and the library's other callers use.
#[test]
fn without_a_session_path_the_job_writes_nothing() {
    let workspace = Workspace::new("none");
    let report = job().run().expect("the job runs");
    assert!(report.session_path.is_none());
    assert!(!report.ses.is_empty(), "the text is still produced");
    assert!(workspace.entries().is_empty(), "{:?}", workspace.entries());
}

/// An unwritable destination is an error the caller sees, not a silent loss — and it is
/// reported before the referee spends anything on a board nobody can be handed.
#[test]
fn an_unwritable_destination_is_a_write_error() {
    let workspace = Workspace::new("unwritable");
    // A directory that does not exist: `rename` and `write` both fail, and the message has to
    // name the path the caller asked for rather than the temporary.
    let ses = workspace.path("nowhere").join("out.ses");
    // `JobReport` is deliberately not `Debug` (it holds the whole design), so the error is
    // taken by hand rather than with `expect_err`.
    let message = match job().with_session_path(&ses).run() {
        Ok(_) => panic!("writing into a missing directory must fail"),
        Err(error) => error.to_string(),
    };
    assert!(
        message.contains(&ses.display().to_string()),
        "the message names the destination: {message}"
    );
    assert!(
        !Path::new(&ses).exists(),
        "nothing was left in the destination"
    );
}
