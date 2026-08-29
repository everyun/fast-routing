//! The G2.1 referee: judge a `(dsn, [ses])` pair with *our* checker, identically for both tools.
//!
//! ```text
//! g21-referee <in.dsn>            -> the imported board, nothing applied  (the gate's baseline)
//! g21-referee <in.dsn> <out.ses>  -> the board that session describes
//! ```
//!
//! One JSON object on stdout, which `bench/tools/g21_judge.py` reads. `bench/README.md`
//! ("Judging a session") is the normative description of the method; this file is its
//! implementation and the two must be changed together.
//!
//! # The session overlay
//!
//! A session is *not* read into the full DSN import. `read_session` merges into whatever board
//! it is handed and both tools re-emit the input's pre-existing copper, so that would insert
//! every pre-existing trace twice — 192 judged violations become 322 on `Issue093`. The target
//! is [`fr_io::specctra::strip_session_copper`]: the same DSN with exactly the copper a session
//! carries removed, and `(type fix)` (`SYSTEM_FIXED`) copper left in place because no session
//! file can carry it. That module documents the fixture that forces the distinction.
//!
//! The baseline (`<in.dsn>` alone) is the *unmodified* import: it is the board as delivered, and
//! `added_violations = judged − baseline` is only meaningful if the baseline is the real board.
//!
//! Import options are [`ImportOptions::default`], i.e. what `RoutingJob` uses for `-do`, so the
//! baseline and both judged boards come from one importer configuration.

use std::process::exit;

use fr_board::item::ItemKind;
use fr_check::score::ScoreWeights;
use fr_core::Referee;
use fr_io::specctra::{import, read_session, strip_session_copper, SesLibrary, SesTarget};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.len() > 2 {
        eprintln!("usage: g21-referee <in.dsn> [out.ses]");
        exit(2);
    }
    let dsn = match std::fs::read(&args[0]) {
        Ok(bytes) => bytes,
        Err(error) => {
            println!("{{\"error\":\"read dsn: {error}\"}}");
            exit(1);
        }
    };
    let judging_session = args.len() == 2;
    let target = if judging_session {
        strip_session_copper(&dsn)
    } else {
        dsn
    };
    let mut design = match import(&target) {
        Ok(design) => design,
        Err(error) => {
            println!("{{\"error\":\"import: {error}\"}}");
            exit(1);
        }
    };

    // Copper the overlay kept because no session file can carry it (see the module docs). Zero
    // on every fixture but `Issue230`, and the number to look at first when a judged board and
    // the router's own in-process line disagree.
    let carried_over = if judging_session {
        count_copper(&design).0
    } else {
        0
    };

    let mut wires_imported = 0usize;
    let mut vias_imported = 0usize;
    let mut ses_errors = 0usize;
    if let Some(path) = args.get(1) {
        let ses = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) => {
                println!("{{\"error\":\"read ses: {error}\"}}");
                exit(1);
            }
        };
        let library = SesLibrary::from_design(&design);
        match read_session(&ses, SesTarget::for_design(&mut design, &library)) {
            Ok(summary) => {
                wires_imported = summary.wires_imported;
                vias_imported = summary.vias_imported;
                ses_errors = summary.errors_encountered;
            }
            Err(error) => {
                println!("{{\"error\":\"read_session: {error:?}\"}}");
                exit(1);
            }
        }
    }

    let verdict = Referee::judge(&design, &ScoreWeights::V2);

    // The gate excludes violations where *both* sides are USER_FIXED items: those are a property
    // of the input board that neither router may be charged for.
    let store = design.board.store();
    let mut both_user_fixed = 0usize;
    for violation in &verdict.violations {
        let a = store.get(violation.a).map(|i| i.fixed_state.is_user_fixed());
        let b = store.get(violation.b).map(|i| i.fixed_state.is_user_fixed());
        if a == Some(true) && b == Some(true) {
            both_user_fixed += 1;
        }
    }

    let (copper_items, nets_with_copper) = count_copper(&design);
    // Nets the ratsnest has something to connect. `max_connections == 0` is a net with one pin
    // (or none), which no router can be asked to route — the anti-degeneracy guard counts these.
    let multi_pin = verdict
        .connectivity
        .nets
        .iter()
        .filter(|net| net.max_connections > 0)
        .count();

    println!(
        "{{\"violations\":{},\"violations_both_user_fixed\":{},\"violations_charged\":{},\
         \"incomplete\":{},\"max_connections\":{},\"score\":{:.4},\
         \"wires_imported\":{},\"vias_imported\":{},\"ses_errors\":{},\
         \"copper_items\":{},\"nets_with_copper\":{},\"nets_multi_pin\":{},\
         \"overlay_carried_over\":{}}}",
        verdict.stats.clearance_violation_count,
        both_user_fixed,
        verdict.stats.clearance_violation_count as usize - both_user_fixed,
        verdict.stats.incomplete_count,
        verdict.stats.max_connections,
        verdict.score.normalized,
        wires_imported,
        vias_imported,
        ses_errors,
        copper_items,
        nets_with_copper,
        multi_pin,
        carried_over,
    );
}

/// `(trace + via count, number of distinct *real* nets carrying one)`.
///
/// Net 0 is "no net", so copper carrying only it is not routing and must not satisfy the
/// anti-degeneracy guard. Nothing in a session file can produce it today — `SesWriter.writeNet`
/// walks nets `1..=max` and the reader takes the net from the enclosing `(net …)` scope — but
/// the guard is a gate, and a gate does not rely on that staying true.
fn count_copper(design: &fr_io::specctra::ImportedDesign) -> (usize, usize) {
    let mut nets: std::collections::BTreeSet<i32> = Default::default();
    let mut items = 0usize;
    for (_, item) in design.board.store().iter() {
        if matches!(item.kind, ItemKind::Trace(_) | ItemKind::Via(_)) {
            items += 1;
            nets.extend(item.nets.iter().copied().filter(|net| *net > 0));
        }
    }
    (items, nets.len())
}
