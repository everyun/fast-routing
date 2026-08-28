//! Hand-computed cases pinning the **metric**.
//!
//! Every number in this file was worked out on paper from
//! `drc-connectivity.md` §1.5 and §2.1 before the code was run, and the
//! derivations are written out in the comments. If one of these fails the
//! referee is measuring something other than what Freerouting measures, and no
//! amount of downstream agreement makes up for it.

mod support;

use fr_board::item::LayerRange;
use fr_check::{check_clearance, TileShape, UniformClearance};
use fr_geometry::planar::{IntBox, IntPoint};
use support::{pin_item, straight_trace, Scaffold};

/// Two parallel traces separated by exactly the required clearance.
///
/// ```text
/// trace A copper  = box(0,  0, 100, 20)   (centre y=10, half width 10)
/// trace B copper  = box(0, 40, 100, 60)   (centre y=50, half width 10)
/// gap             = 20 = the required clearance
/// c1 = c2         = round(0.5 * 20) = 10
/// A.enlarge(10)   -> top_y    = 20 + 10 = 30
/// B.enlarge(10)   -> bottom_y = 40 - 10 = 30
/// intersection    -> bottom_y == top_y == 30, left_x != right_x  =>  dimension 1
/// ```
///
/// `dimension() == 1` is not a violation: exactly touching is legal.
#[test]
fn traces_at_exactly_the_required_clearance_touch_legally() {
    let mut board = Scaffold::new(1);
    let (a, a_shape) = straight_trace(&[1], 0, IntPoint::new(0, 10), IntPoint::new(100, 10), 10);
    let (b, b_shape) = straight_trace(&[2], 0, IntPoint::new(0, 50), IntPoint::new(100, 50), 10);
    assert_eq!(a_shape, TileShape::Box(IntBox::new(0, 0, 100, 20)));
    assert_eq!(b_shape, TileShape::Box(IntBox::new(0, 40, 100, 60)));
    board.insert_one(a, a_shape.clone(), 0);
    board.insert_one(b, b_shape.clone(), 0);

    // The metric, directly.
    let inter = a_shape.enlarge(10.0).intersection(&b_shape.enlarge(10.0));
    assert_eq!(inter.dimension(), 1, "enlarged shapes touch, they do not overlap");

    // And through the checker.
    let rules = UniformClearance(20);
    assert!(check_clearance(&board.view(&rules)).is_empty());
}

/// The same pair moved one board unit closer.
///
/// ```text
/// trace B copper  = box(0, 39, 100, 59)
/// A.enlarge(10)   -> top_y    = 30
/// B.enlarge(10)   -> bottom_y = 29
/// intersection    -> a 1-unit-tall band  =>  dimension 2  =>  violation
/// ```
///
/// The bisection then reports the gap. The true gap is 19 board units, and the
/// bisection is a lower bound on it that is exact to about one unit.
#[test]
fn one_unit_closer_is_a_violation() {
    let mut board = Scaffold::new(1);
    let (a, a_shape) = straight_trace(&[1], 0, IntPoint::new(0, 10), IntPoint::new(100, 10), 10);
    let (b, b_shape) = straight_trace(&[2], 0, IntPoint::new(0, 49), IntPoint::new(100, 49), 10);
    assert_eq!(b_shape, TileShape::Box(IntBox::new(0, 39, 100, 59)));
    let id_a = board.insert_one(a, a_shape.clone(), 0);
    let id_b = board.insert_one(b, b_shape.clone(), 0);

    assert_eq!(
        a_shape.enlarge(10.0).intersection(&b_shape.enlarge(10.0)).dimension(),
        2
    );

    let rules = UniformClearance(20);
    let violations = check_clearance(&board.view(&rules));
    assert_eq!(violations.len(), 1, "{violations:#?}");
    let v = &violations[0];
    assert_eq!((v.a, v.b), (id_a, id_b));
    assert_eq!(v.layer, 0);
    assert_eq!(v.expected, 20);
    assert!(!v.actual_dim2, "the raw copper does not overlap");
    assert!(
        (v.actual - 19.0).abs() <= 1.0,
        "bisection should land on the 19-unit gap, got {}",
        v.actual
    );
    assert!(v.shortfall() > 0.0);
}

/// Copper on copper: the bisection short-circuits and the gap is a fact, not a
/// bound.
#[test]
fn overlapping_copper_reports_a_zero_gap_and_flags_it() {
    let mut board = Scaffold::new(1);
    let (a, a_shape) = straight_trace(&[1], 0, IntPoint::new(0, 10), IntPoint::new(100, 10), 10);
    let (b, b_shape) = straight_trace(&[2], 0, IntPoint::new(0, 15), IntPoint::new(100, 15), 10);
    board.insert_one(a, a_shape, 0);
    board.insert_one(b, b_shape, 0);

    let rules = UniformClearance(20);
    let violations = check_clearance(&board.view(&rules));
    assert_eq!(violations.len(), 1);
    assert!(violations[0].actual_dim2);
    assert_eq!(violations[0].actual, 0.0);
}

/// **The key case.** A pair whose offset direction sits between the axis and the
/// 45° diagonal, where the octagon norm and the Euclidean distance give
/// different verdicts.
///
/// Two point-sized pads at `(0, 0)` and `(100, 40)`, required clearance 102, so
/// `c1 = c2 = 51`.
///
/// ```text
/// round(51)           = 51
/// round(sqrt(2) * 51) = round(72.125) = 72
///
/// A = point(0, 0)     -> octagon (0,0,0,0, 0,0, 0,0)
///     enlarged        -> (-51, -51, 51, 51,  -72,  72,  -72,  72)
/// B = point(100, 40)  -> octagon (100,40,100,40, 60,60, 140,140)
///     enlarged        -> ( 49, -11,151, 91,  -12, 132,   68, 212)
///
/// intersection bounds:
///   left/right     max(-51, 49) =  49  <  51 = min( 51, 151)   OK
///   bottom/top     max(-51,-11) = -11  <  51 = min( 51,  91)   OK
///   ul/lr diagonal max(-72,-12) = -12  <  72 = min( 72, 132)   OK
///   ll/ur diagonal max(-72, 68) =  68  <  72 = min( 72, 212)   OK   <-- the tight one
/// all four strict  =>  dimension 2  =>  VIOLATION
/// ```
///
/// The Euclidean gap is `sqrt(100² + 40²) = 107.70` board units, comfortably
/// more than the required 102 — a Euclidean checker would call this pair legal
/// and would under-report the board. The octagon's unit ball contains the disc
/// and reaches `sec(22.5°) = 1.0824` times as far at 22.5°, which is the
/// direction `(100, 40)` is closest to.
#[test]
fn octagon_norm_and_euclid_disagree_off_axis() {
    let a_shape = TileShape::Box(IntBox::new(0, 0, 0, 0));
    let b_shape = TileShape::Box(IntBox::new(100, 40, 100, 40));

    let euclid = ((100.0_f64).powi(2) + (40.0_f64).powi(2)).sqrt();
    assert!(euclid > 102.0, "Euclid would call this legal: {euclid}");

    assert_eq!(
        a_shape.enlarge(51.0).intersection(&b_shape.enlarge(51.0)).dimension(),
        2,
        "the octagon norm calls it a violation"
    );

    let mut board = Scaffold::new(1);
    let id_a = board.insert_one(
        pin_item(&[1], IntPoint::new(0, 0), LayerRange::single(0)),
        a_shape,
        0,
    );
    let id_b = board.insert_one(
        pin_item(&[2], IntPoint::new(100, 40), LayerRange::single(0)),
        b_shape,
        0,
    );
    let rules = UniformClearance(102);
    let violations = check_clearance(&board.view(&rules));
    assert_eq!(violations.len(), 1, "{violations:#?}");
    assert_eq!((violations[0].a, violations[0].b), (id_a, id_b));
}

/// The same disagreement in the *other* direction, at exactly 45°, caused by the
/// integer rounding of the diagonal offset.
///
/// Pads at `(0, 0)` and `(100, 100)`; Euclidean gap `141.42`.
///
/// ```text
/// clearance 142 -> c = 71, round(sqrt(2) * 71) = round(100.409) = 100
///   A.urx =   0 + 100 = 100      B.llx = 200 - 100 = 100
///   100 > 100 is false  =>  the ll/ur bounds are equal  =>  dimension 1  =>  legal
///   Euclid: 141.42 < 142  =>  a Euclidean checker calls it a violation
///
/// clearance 144 -> c = 72, round(sqrt(2) * 72) = round(101.823) = 102
///   A.urx =   0 + 102 = 102      B.llx = 200 - 102 =  98
///   102 > 98  =>  dimension 2  =>  violation
/// ```
///
/// So the two metrics disagree in both directions and the boundary is decided by
/// `round`, which is why the spec insists the offsets be quantised exactly as
/// upstream quantises them.
#[test]
fn at_exactly_45_degrees_rounding_decides_the_verdict() {
    let a = TileShape::Box(IntBox::new(0, 0, 0, 0));
    let b = TileShape::Box(IntBox::new(100, 100, 100, 100));

    let euclid = (2.0_f64 * 100.0 * 100.0).sqrt();
    assert!((euclid - 141.421_356).abs() < 1e-6);

    assert!(euclid < 142.0, "Euclid: violation at clearance 142");
    assert_eq!(
        a.enlarge(71.0).intersection(&b.enlarge(71.0)).dimension(),
        1,
        "octagon norm: legal at clearance 142"
    );

    assert_eq!(
        a.enlarge(72.0).intersection(&b.enlarge(72.0)).dimension(),
        2,
        "octagon norm: violation at clearance 144"
    );
}

/// A pair overlapping on three layers is three violations; a pair overlapping in
/// several places on one layer is one (`drc-connectivity.md` §2.7).
#[test]
fn violations_are_counted_per_layer_and_deduplicated_within_one() {
    let mut board = Scaffold::new(3);
    let pad = IntBox::new(0, 0, 100, 100);
    let id_a = board.insert(
        pin_item(&[1], IntPoint::new(50, 50), LayerRange::new(0, 2)),
        (0..3)
            .map(|l| fr_check::ItemShape::new(pad, l))
            .collect(),
    );
    let id_b = board.insert(
        pin_item(&[2], IntPoint::new(50, 50), LayerRange::new(0, 2)),
        (0..3)
            .map(|l| fr_check::ItemShape::new(pad, l))
            .collect(),
    );
    let rules = UniformClearance(10);
    let violations = check_clearance(&board.view(&rules));
    assert_eq!(violations.len(), 3);
    assert_eq!(
        violations.iter().map(|v| v.layer).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert!(violations.iter().all(|v| (v.a, v.b) == (id_a, id_b)));

    // Now give one of them five separate overlapping pieces on layer 0: still
    // one violation, because the dedup key is (min id, max id, layer).
    let mut board = Scaffold::new(1);
    let id_a = board.insert_one(
        pin_item(&[1], IntPoint::new(50, 50), LayerRange::single(0)),
        IntBox::new(0, 0, 500, 100),
        0,
    );
    let id_b = board.insert(
        pin_item(&[2], IntPoint::new(50, 50), LayerRange::single(0)),
        (0..5)
            .map(|i| fr_check::ItemShape::new(IntBox::new(i * 100, 0, i * 100 + 50, 100), 0))
            .collect(),
    );
    let violations = check_clearance(&board.view(&rules));
    assert_eq!(violations.len(), 1);
    assert_eq!((violations[0].a, violations[0].b), (id_a, id_b));
}

/// A clearance of zero still reports bare copper overlap, and never reports
/// merely-touching copper.
#[test]
fn zero_clearance_still_catches_copper_on_copper() {
    let mut board = Scaffold::new(1);
    board.insert_one(
        pin_item(&[1], IntPoint::new(0, 0), LayerRange::single(0)),
        IntBox::new(0, 0, 100, 100),
        0,
    );
    board.insert_one(
        pin_item(&[2], IntPoint::new(0, 0), LayerRange::single(0)),
        IntBox::new(99, 0, 200, 100),
        0,
    );
    let rules = UniformClearance(0);
    assert_eq!(check_clearance(&board.view(&rules)).len(), 1);

    let mut board = Scaffold::new(1);
    board.insert_one(
        pin_item(&[1], IntPoint::new(0, 0), LayerRange::single(0)),
        IntBox::new(0, 0, 100, 100),
        0,
    );
    board.insert_one(
        pin_item(&[2], IntPoint::new(0, 0), LayerRange::single(0)),
        IntBox::new(100, 0, 200, 100),
        0,
    );
    assert!(check_clearance(&board.view(&rules)).is_empty());
}
