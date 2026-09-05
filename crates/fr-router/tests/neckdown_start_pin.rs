//! Start-pin neckdown (`MazeSearchAlgo.java:361-369`).
//!
//! easyeda-pcb1 is an EasyEDA Pro export whose pads are all narrower than the
//! net class trace width, so every pad escape only fits necked down. Without
//! the start-pin branch the maze judges the start room *thin* at full width,
//! runs the full-width shove check between the pin centres, fails it, and the
//! frontier exhausts after one expansion (`REPORT-easyeda` §1, net `$1N2152`,
//! pins u1-6e8 → u1-7e7). With the branch the pin's own neckdown half width
//! (1589 internal units) caps `half_width` first, the room is thick, and the
//! connection routes.
//!
//! The fixture is out-of-suite (`bench/fixtures-oos/`, not fetched by
//! `bench/fetch_fixtures.sh`); the tests skip when it is absent.

use std::path::PathBuf;

use fr_io::specctra;
use fr_router::ctrl::AutorouteControl;
use fr_router::engine::AutorouteEngine;
use fr_router::maze::search::MazeSearch;
use fr_rules::router_settings::{BoardAspectRatio, RouterSettings};

fn fixture() -> Option<PathBuf> {
    let dir = std::env::var_os("FR_OOS_FIXTURE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bench/fixtures-oos"));
    let path = dir.join("easyeda-pcb1.dsn");
    if path.exists() {
        return Some(path);
    }
    eprintln!("skipping: {} not found", path.display());
    None
}

fn load(neckdown: bool) -> Option<(specctra::ImportedDesign, RouterSettings)> {
    let bytes = std::fs::read(fixture()?).expect("the fixture is readable");
    let design = specctra::import(&bytes).expect("the fixture imports");
    let mut settings = RouterSettings::default();
    settings.set_layer_count(design.rules.layer_structure.len());
    settings.apply_board_specific_optimizations(
        BoardAspectRatio::from_bounding_box(
            f64::from(design.bounds.width()),
            f64::from(design.bounds.height()),
        ),
        &design.rules.layer_structure,
    );
    settings.automatic_neckdown = neckdown;
    Some((design, settings))
}

/// The two pins of net `$1N2152`, in store order.
fn n2152_pins(design: &specctra::ImportedDesign) -> Vec<fr_board::item::ItemId> {
    let net = design
        .rules
        .nets
        .get_by_name_and_subnet("$1N2152", 1)
        .expect("net $1N2152 exists");
    design
        .board
        .store()
        .iter_ascending()
        .filter(|(_, item)| {
            matches!(item.kind, fr_board::item::ItemKind::Pin(_))
                && item.contains_net(net.net_number)
        })
        .map(|(id, _)| id)
        .collect()
}

#[test]
fn start_pin_neckdown_routes_the_easyeda_pad_escape() {
    let Some((design, settings)) = load(true) else {
        return;
    };
    let pins = n2152_pins(&design);
    assert_eq!(pins.len(), 2, "net $1N2152 has exactly the two pins");

    let ctrl = AutorouteControl::for_net(
        &design.board,
        &design.rules,
        &settings,
        design.rules.nets.get_by_name_and_subnet("$1N2152", 1).unwrap().net_number,
    );
    assert!(ctrl.with_neckdown, "the setting must reach the control");

    let mut engine = AutorouteEngine::new(&design.board, &design.rules, ctrl);
    // The mechanism, pinned: the pad's neckdown half width is the value the
    // diagnostic measured (0.5 × 3180 − 1 = 1589).
    let pin_layer = {
        let item = design.board.store().get(pins[0]).expect("pin");
        let fr_board::item::ItemKind::Pin(pin) = &item.kind else {
            panic!("pins[0] is a pin");
        };
        pin.layers.first
    };
    assert_eq!(
        engine.pin_trace_neckdown_halfwidth(pins[0], pin_layer),
        Some(1589),
        "Pin.get_trace_neckdown_halfwidth on the pad layer"
    );

    let mut search = MazeSearch::get_instance(&mut engine, &pins[..1], &pins[1..])
        .expect("start and destination");
    search
        .find_connection()
        .expect("the start-pin neckdown branch makes the escape routable");
}

#[test]
fn without_neckdown_the_same_connection_exhausts() {
    let Some((design, settings)) = load(false) else {
        return;
    };
    let pins = n2152_pins(&design);
    let ctrl = AutorouteControl::for_net(
        &design.board,
        &design.rules,
        &settings,
        design.rules.nets.get_by_name_and_subnet("$1N2152", 1).unwrap().net_number,
    );
    assert!(!ctrl.with_neckdown);
    let mut engine = AutorouteEngine::new(&design.board, &design.rules, ctrl);
    let mut search = MazeSearch::get_instance(&mut engine, &pins[..1], &pins[1..])
        .expect("start and destination");
    assert!(
        search.find_connection().is_err(),
        "without the neckdown cap the full-width shove check must still fail \
         (the control that proves the branch is load-bearing)"
    );
}
