//! Diagnostic probe for the RelayModule D0 dead-pocket (wave B handoff):
//! fanout net /CAN_RX (net 39), pin U1-32 at (1728737,-2069130) — our maze
//! dies after one expansion (`start_rooms=1, expansions=1, exhausted`) while
//! the jar escapes. Dumps the start room's completion and its door set.

use std::path::PathBuf;

use fr_io::specctra;
use fr_router::ctrl::AutorouteControl;
use fr_router::engine::AutorouteEngine;
use fr_router::maze::search::MazeSearch;
use fr_rules::router_settings::{BoardAspectRatio, RouterSettings};

fn load() -> (specctra::ImportedDesign, RouterSettings) {
    let bytes = std::fs::read(PathBuf::from(
        "/tmp/freerouting-upstream/fixtures/Issue110-RelayModule.dsn",
    ))
    .expect("fixture");
    let design = specctra::import(&bytes).expect("imports");
    let mut settings = RouterSettings::default();
    settings.set_layer_count(design.rules.layer_structure.len());
    settings.apply_board_specific_optimizations(
        BoardAspectRatio::from_bounding_box(
            f64::from(design.bounds.width()),
            f64::from(design.bounds.height()),
        ),
        &design.rules.layer_structure,
    );
    (design, settings)
}

#[test]
fn dump_d0_start_room_doors() {
    let (design, settings) = load();
    let net = design
        .rules
        .nets
        .get_by_name_and_subnet("/CAN_RX", 1)
        .expect("net /CAN_RX");
    eprintln!("/CAN_RX net_number = {}", net.net_number);
    // The pin under test and the whole-net target set.
    let mut starts = Vec::new();
    let mut targets = Vec::new();
    for (id, item) in design.board.store().iter_ascending() {
        if !item.contains_net(net.net_number) {
            continue;
        }
        let fr_board::item::ItemKind::Pin(pin) = &item.kind else {
            targets.push(id);
            continue;
        };
        if pin.center == fr_geometry::planar::IntPoint::new(1728737, -2069130) {
            starts.push(id);
            eprintln!(
                "start pin: {id:?} padstack={} layers={:?}",
                pin.padstack_no, pin.layers
            );
        } else {
            targets.push(id);
        }
    }
    assert_eq!(starts.len(), 1, "the U1-32 pin");
    assert!(!targets.is_empty());

    let ctrl = AutorouteControl::for_net(&design.board, &design.rules, &settings, net.net_number);
    let mut engine = AutorouteEngine::new(&design.board, &design.rules, ctrl);
    {
        let mut search = MazeSearch::get_instance(&mut engine, &starts, &targets)
            .expect("start and destination");
        let outcome = search.find_connection();
        eprintln!("search outcome: {outcome:?}");
    }

    // Dump every room within ~200k of the pin and its doors.
    let pin = (1728737, -2069130);
    let mut dumped = 0;
    for room in 0..engine.rooms.room_count() {
        let id = fr_router::rooms::RoomId(room as u32);
        let room = engine.rooms.room(id);
        if !room.alive {
            continue;
        }
        let bb = room.shape.bounding_box();
        let near = |x: i32, y: i32| (x - pin.0).abs() < 250_000 && (y - pin.1).abs() < 250_000;
        if !(near(bb.ll.x, bb.ll.y) || near(bb.ur.x, bb.ur.y)) {
            continue;
        }
        dumped += 1;
        eprintln!(
            "room {id:?} kind={:?} layer={} bbox=[({},{})..({},{})] doors={}",
            room.kind,
            room.layer,
            bb.ll.x,
            bb.ll.y,
            bb.ur.x,
            bb.ur.y,
            room.doors.len()
        );
        for door_id in &room.doors {
            let door = engine.rooms.door(*door_id);
            let db = door.shape.bounding_box();
            let other = door
                .other_room_any(id)
                .map(|o| format!("{o:?}"))
                .unwrap_or_else(|| "none".into());
            eprintln!(
                "  door {door_id:?} kind={:?} dim={} to={} bbox=[({},{})..({},{})]",
                door.kind, door.dimension, other, db.ll.x, db.ll.y, db.ur.x, db.ur.y
            );
        }
    }
    eprintln!("dumped {dumped} rooms near the pin");
}

#[test]
fn replicate_fanout_attempt_and_dump() {
    use fr_router::batch::{connected_set, unconnected_set, ContactScratch};
    use fr_router::fanout::{fanout_control, FanoutConfig, FanoutPin};

    let (design, settings) = load();
    let net = design
        .rules
        .nets
        .get_by_name_and_subnet("/CAN_RX", 1)
        .expect("net");
    let board = design.board.clone();
    let pin_item = starts_of(&design, net.net_number)[0];
    let pin = {
        let item = board.store().get(pin_item).unwrap();
        let fr_board::item::ItemKind::Pin(p) = &item.kind else {
            panic!("pin");
        };
        FanoutPin {
            item: pin_item,
            pin_no: p.pin_no,
            center: p.center,
            layer: 0,
            net_no: net.net_number,
            distance_to_component_center: 0.0,
            distance_to_closest_on_net: f64::MAX,
            surroundings_density: 0,
        }
    };
    let um_per_board_unit = specctra::Unit::scale(1.0, design.unit, specctra::Unit::Um)
        / f64::from(design.resolution.max(1));
    let config = FanoutConfig::from_settings(&settings, 1000.0 / um_per_board_unit);
    eprintln!(
        "annulus: min={:.0} max={:.0}",
        config.min_escape_length, config.max_escape_length
    );
    let mut scratch = ContactScratch::new();
    let connected = connected_set(&board, net.net_number, pin_item, &mut scratch);
    let targets = unconnected_set(&board, net.net_number, &connected);
    eprintln!("connected={connected:?} targets={targets:?}");

    let ctrl = fanout_control(&board, &design.rules, &settings, &config, &pin, 100);
    let mut engine = AutorouteEngine::new(&board, &design.rules, ctrl);
    {
        let mut search =
            MazeSearch::get_instance(&mut engine, &connected, &targets[..1]).expect("start/dest");
        let outcome = search.find_connection();
        eprintln!("closest-target outcome ok={}", outcome.is_ok());
        if outcome.is_err() {
            // Dump the start room + its doors on the failed path.
            for room in 0..engine.rooms.room_count() {
                let id = fr_router::rooms::RoomId(room as u32);
                let room = engine.rooms.room(id);
                if !room.alive {
                    continue;
                }
                let bb = room.shape.bounding_box();
                eprintln!(
                    "room {id:?} kind={:?} bbox=[({},{})..({},{})] doors={}",
                    room.kind,
                    bb.ll.x,
                    bb.ll.y,
                    bb.ur.x,
                    bb.ur.y,
                    room.doors.len()
                );
                for door_id in &room.doors {
                    let door = engine.rooms.door(*door_id);
                    let db = door.shape.bounding_box();
                    eprintln!(
                        "  door {door_id:?} kind={:?} dim={} to={:?} bbox=[({},{})..({},{})]",
                        door.kind,
                        door.dimension,
                        door.other_room_any(id),
                        db.ll.x,
                        db.ll.y,
                        db.ur.x,
                        db.ur.y
                    );
                }
            }
        }
    }
}

fn starts_of(design: &specctra::ImportedDesign, net_no: i32) -> Vec<fr_board::item::ItemId> {
    design
        .board
        .store()
        .iter_ascending()
        .filter(|(_, item)| {
            matches!(&item.kind, fr_board::item::ItemKind::Pin(p) if p.center == fr_geometry::planar::IntPoint::new(1728737, -2069130))
                && item.contains_net(net_no)
        })
        .map(|(id, _)| id)
        .collect()
}

#[test]
fn probe_issue107_incompletes() {
    let bytes = std::fs::read(PathBuf::from(
        "/tmp/freerouting-upstream/fixtures/Issue107-freq_teiler_200kHz_kicad.dsn",
    ))
    .expect("fixture");
    let mut design = specctra::import(&bytes).expect("imports");
    let ses_bytes = std::fs::read("/tmp/107_full.ses").expect("ses");
    let library = specctra::SesLibrary::from_design(&design);
    specctra::read_session(
        &ses_bytes,
        specctra::SesTarget::for_design(&mut design, &library),
    )
    .expect("read_session");

    let shapes = fr_check::shape_store(&design.board);
    let view = fr_check::view::StoreView::new(
        design.board.store(),
        &shapes,
        &design.rules.clearance_matrix,
        design.board.layers().len(),
    );
    let conn = fr_check::connectivity::board_connectivity(&view);
    eprintln!("Issue107 full incomplete_count={}", conn.incomplete_count);
    for net in &conn.nets {
        if net.incompletes > 0 {
            eprintln!(
                "Net {} (name: {:?}) has {} incompletes out of {}",
                net.net,
                design.rules.nets.get(net.net).map(|n| &n.name),
                net.incompletes,
                net.max_connections
            );
            for a in &net.airlines {
                eprintln!(
                    "  airline from={:?} ({:?}) to={:?} ({:?})",
                    a.from_item, a.from, a.to_item, a.to
                );
            }
        }
    }
}
