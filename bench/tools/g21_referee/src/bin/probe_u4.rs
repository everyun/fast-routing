//! Diagnostic probe (short-hdi task): does the judged board contain U4 pad 7
//! (net CAM_EN, B.Cu), and does the referee's violation list contain the pair
//! (our SS_CTRL trace, that pad)?
//!
//! usage: probe_u4 <in.dsn> <out.ses>

use fr_board::item::ItemKind;
use fr_check::score::ScoreWeights;
use fr_core::Referee;
use fr_io::specctra::{import, read_session, strip_session_copper, SesLibrary, SesTarget};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dsn = std::fs::read(&args[0]).unwrap();
    let ses = std::fs::read(&args[1]).unwrap();
    let target = strip_session_copper(&dsn);
    let mut design = import(&target).unwrap();
    let library = SesLibrary::from_design(&design);
    let summary = read_session(&ses, SesTarget::for_design(&mut design, &library)).unwrap();
    println!(
        "session: wires={} vias={} errors={}",
        summary.wires_imported, summary.vias_imported, summary.errors_encountered
    );

    let net_no_of = |name: &str| {
        design
            .rules
            .nets
            .iter()
            .find(|n| n.name == name)
            .map(|n| n.net_number)
    };
    let cam_en = net_no_of("CAM_EN").expect("CAM_EN net");
    let ss_ctrl = net_no_of("Net-(U4-SS_CTRL)").expect("SS_CTRL net");
    println!("net numbers: CAM_EN={cam_en} SS_CTRL={ss_ctrl}");

    let store = design.board.store();
    // Everything on either net, so the pad row and the trace are both visible.
    for (id, item) in store.iter() {
        if !item.nets.contains(cam_en) && !item.nets.contains(ss_ctrl) {
            continue;
        }
        let (kind, x, y): (&str, f64, f64) = match &item.kind {
            ItemKind::Pin(p) => ("pin", p.center.x as f64, p.center.y as f64),
            ItemKind::Via(v) => ("via", v.center.x as f64, v.center.y as f64),
            ItemKind::Trace(t) => {
                let a = t.first_corner().to_float();
                ("trace", a.x, a.y)
            }
            _ => continue,
        };
        if true {
            println!(
                "item {} {kind} nets={:?} layers={}..{} fixed={:?} first=({x:.0},{y:.0})",
                id,
                item.nets,
                item.first_layer(),
                item.last_layer(),
                item.fixed_state,
            );
        }
    }

    let verdict = Referee::judge(&design, &ScoreWeights::V2);
    println!("judged violations: {}", verdict.violations.len());
    for v in &verdict.violations {
        for id in [v.a, v.b] {
            let item = store.get(id).unwrap();
            if let ItemKind::Pin(_) = &item.kind {
                if true {
                    let other_id = if v.a == id { v.b } else { v.a };
                    let other = store.get(other_id).unwrap();
                    let other_pos = match &other.kind {
                        ItemKind::Pin(p) => format!("pin ({},{})", p.center.x, p.center.y),
                        ItemKind::Via(via) => format!("via ({},{})", via.center.x, via.center.y),
                        ItemKind::Trace(t) => {
                            let a = t.first_corner().to_float();
                            let b = t.last_corner().to_float();
                            format!("trace ({:.0},{:.0})->({:.0},{:.0})", a.x, a.y, b.x, b.y)
                        }
                        k => format!("{k:?}"),
                    };
                    println!(
                        "VIOLATION pin {} nets={:?} vs {} nets={:?} layer={} expected={} actual={:.1}",
                        id,
                        item.nets,
                        other_pos,
                        other.nets,
                        v.layer,
                        v.expected,
                        v.actual,
                    );
                }
            }
        }
    }
}
