//! Probe: after import + fanout, dump net 6's members, geometry and the
//! connectivity components, for the Issue039 early-pass investigation.
use fr_io::specctra::import;

fn kind_name(item: &fr_board::item::Item) -> &'static str {
    match &item.kind {
        fr_board::item::ItemKind::Pin(_) => "Pin",
        fr_board::item::ItemKind::Via(_) => "Via",
        fr_board::item::ItemKind::Trace(_) => "Trace",
        fr_board::item::ItemKind::ConductionArea(_) => "ConductionArea",
        _ => "other",
    }
}

fn main() {
    let dsn = std::env::args().nth(1).expect("dsn path");
    let bytes = std::fs::read(&dsn).unwrap();
    let mut design = import(&bytes).unwrap();

    // Fanout, exactly as the router stage does: the pipeline's settings_for.
    let mut settings = fr_rules::router_settings::RouterSettings::default();
    settings.set_layer_count(design.rules.layer_structure.len());
    if let Some(dsn) = &design.autoroute_settings {
        fr_io::specctra::apply_autoroute_settings(&mut settings, dsn);
    }
    settings.apply_board_specific_optimizations(
        fr_rules::router_settings::BoardAspectRatio::from_bounding_box(
            f64::from(design.bounds.width()),
            f64::from(design.bounds.height()),
        ),
        &design.rules.layer_structure,
    );
    let scale = 10000.0;
    let config = fr_router::fanout::FanoutConfig::from_settings(&settings, scale);
    let outcome =
        fr_router::fanout::fanout_board(&mut design.board, &design.rules, &settings, &config);
    println!("fanout: routed={}", outcome.total_items_fanouted);

    let board = &mut design.board;
    let mut net6 = None;
    for (id, item) in board.store().iter() {
        if let fr_board::item::ItemKind::Pin(p) = &item.kind {
            if p.center.x == 1574800 && p.center.y == -838200 {
                println!(
                    "#191 details: layers={:?} padstack_no={}",
                    p.layers, p.padstack_no
                );
            }
            if (p.center.x == 1656670 && p.center.y == -806525)
                || (p.center.x == 1574800 && p.center.y == -838200)
            {
                println!(
                    "seed pin {:?} center=({},{}) nets={:?}",
                    id, p.center.x, p.center.y, item.nets
                );
                net6 = Some(item.nets[0]);
            }
        }
    }
    let net6 = net6.expect("net-6 seed pins not found");
    println!("net_no = {net6}");

    let members: Vec<(u32, String)> = board
        .store()
        .net_items(net6)
        .iter()
        .map(|id| {
            let item = board.store().get(*id).unwrap();
            let geo = match &item.kind {
                fr_board::item::ItemKind::Pin(p) => format!("Pin@{},{}", p.center.x, p.center.y),
                fr_board::item::ItemKind::Via(v) => format!("Via@{},{}", v.center.x, v.center.y),
                fr_board::item::ItemKind::Trace(t) => format!(
                    "Trace {:?}..{:?}",
                    t.polyline.first_corner().to_float(),
                    t.polyline.last_corner().to_float()
                ),
                other => format!("{other:?}"),
            };
            (
                id.raw(),
                format!(
                    "#{} {} nets={:?} fixed={:?}",
                    id.raw(),
                    geo,
                    item.nets,
                    item.fixed_state
                ),
            )
        })
        .collect();
    println!("net {net6} has {} members:", members.len());
    for m in &members {
        println!("  {}", m.1);
    }

    let n = board.net_component_count(net6);
    println!("components = {n}");

    for seed in ["1656670", "1574800"] {
        let raw = members
            .iter()
            .find(|m| m.1.contains(seed))
            .map(|m| m.0)
            .expect("seed in members");
        let id = fr_board::ItemId::from_raw(raw);
        let item = board.store().get(id).unwrap();
        if let fr_board::item::ItemKind::Pin(p) = &item.kind {
            let mut contacts = Vec::new();
            board.normal_contacts_at(
                id,
                &fr_geometry::planar::Point::Int(p.center),
                &mut contacts,
            );
            println!("normal contacts of #{raw} ({:?}):", p.center);
            for c in &contacts {
                let ci = board.store().get(*c).unwrap();
                println!("  -> #{} {}", c.raw(), kind_name(ci));
            }
            if contacts.is_empty() {
                println!("  (none)");
            }
        }
    }
    dump_unescaped(board);
}

// The task's follow-up: list every SMD pin the fanout left without contacts.
#[allow(dead_code)]
fn dump_unescaped(board: &fr_board::Board) {
    let mut unescaped = Vec::new();
    for (id, item) in board.store().iter() {
        if let fr_board::item::ItemKind::Pin(p) = &item.kind {
            if !p.layers.is_single() {
                continue; // through-hole: not a fanout target
            }
            let mut contacts = Vec::new();
            board.normal_contacts_at(
                id,
                &fr_geometry::planar::Point::Int(p.center),
                &mut contacts,
            );
            if contacts.is_empty() {
                unescaped.push(format!(
                    "#{} @{},{} nets={:?}",
                    id.raw(),
                    p.center.x,
                    p.center.y,
                    item.nets
                ));
            }
        }
    }
    println!("unescaped single-layer pins: {}", unescaped.len());
    for u in &unescaped {
        println!("  {}", u);
    }
}
