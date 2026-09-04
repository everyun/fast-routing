//! Diagnostic probe (short-hdi task): dump every judged violation as a stable
//! identity line — (kind, rounded position, nets) pairs — so a baseline run
//! and a session run can be diffed as *sets* (item ids are not comparable
//! across the two imports).
//!
//! usage: probe_pairs <in.dsn> [out.ses]

use fr_board::item::{Item, ItemId, ItemKind};
use fr_check::score::ScoreWeights;
use fr_core::Referee;
use fr_io::specctra::{import, read_session, strip_session_copper, SesLibrary, SesTarget};

fn ident(store: &fr_board::store::ItemStore, id: ItemId) -> String {
    let item: &Item = store.get(id).unwrap();
    match &item.kind {
        ItemKind::Pin(p) => format!("pin({},{})", p.center.x, p.center.y),
        ItemKind::Via(v) => format!("via({},{})", v.center.x, v.center.y),
        ItemKind::Trace(t) => {
            let a = t.first_corner().to_float();
            let b = t.last_corner().to_float();
            format!("trace({:.0},{:.0},{:.0},{:.0})", a.x, a.y, b.x, b.y)
        }
        other => format!("{}#{:?}", std::any::type_name_of_val(other), std::mem::discriminant(other)),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dsn = std::fs::read(&args[0]).unwrap();
    let judging = args.len() == 2;
    let target = if judging {
        strip_session_copper(&dsn)
    } else {
        dsn
    };
    let mut design = import(&target).unwrap();
    if judging {
        let ses = std::fs::read(&args[1]).unwrap();
        let library = SesLibrary::from_design(&design);
        read_session(&ses, SesTarget::for_design(&mut design, &library)).unwrap();
    }
    let verdict = Referee::judge(&design, &ScoreWeights::V2);
    let store = design.board.store();
    let mut lines: Vec<String> = verdict
        .violations
        .iter()
        .map(|v| {
            let (mut x, mut y) = (ident(store, v.a), ident(store, v.b));
            if y < x {
                std::mem::swap(&mut x, &mut y);
            }
            let a = store.get(v.a).unwrap();
            let b = store.get(v.b).unwrap();
            let uf = a.fixed_state.is_user_fixed() && b.fixed_state.is_user_fixed();
            format!("L{} E{} A{:.0} uf={} | {} | {}", v.layer, v.expected, v.actual, uf, x, y)
        })
        .collect();
    lines.sort();
    println!("count={}", lines.len());
    for l in lines {
        println!("{l}");
    }
}
