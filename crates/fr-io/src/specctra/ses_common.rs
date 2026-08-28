//! Plumbing shared by the SES writer and the SES reader
//! (`dsn-board-semantics.md` §12).
//!
//! Three things the two halves have to agree on:
//!
//! 1. **Java's rounding.** Every SES coordinate is an integer produced by
//!    `(int) Math.round(double)`, which is [`super::jround::jround`] and *not* Rust's
//!    `f64::round`. `Math.rint` — the tie-to-even sibling that
//!    [`super::ses_write::format_placement_rotation`] needs — is [`java_rint`] below.
//! 2. **The session scale factor**, which is a *different* number from the DSN scale factor
//!    (§3.5): `dsnToBoard(1) / resolution`, and the writer *divides* board coordinates by it.
//! 3. **The padstack-name binding.** `fr_board::Padstack` has no name — the board's padstack
//!    table is an anonymous interning of *placed* pad geometry ([`super::build`]'s
//!    `PadstackKey`). SES needs the DSN name on both sides: `library_out` declares padstacks by
//!    name and `(via <name> …)` references them. [`SesLibrary`] is that binding.

use fr_board::{Board, Padstack as BoardPadstack, TileShape};
use fr_rules::{PadShape, Padstack as RulesPadstack};

use super::build::{ImportedDesign, ImportedPadstack, PadGeometry};
use super::strip_numeric_suffix;

// ---------------------------------------------------------------------------------------------
// Java numerics
// ---------------------------------------------------------------------------------------------

/// `java.lang.Math.rint(double)` — round to nearest, ties to **even**.
///
/// Only [`super::ses_write::format_placement_rotation`] needs it, and only on values in
/// `[0, 360000]`, but it is spelled out here rather than approximated because the tie rule is
/// what decides whether `0.0005°` prints as `0` or as `0.001`.
pub fn java_rint(value: f64) -> f64 {
    if !value.is_finite() {
        return value;
    }
    let floor = value.floor();
    let fraction = value - floor;
    // An exact half goes to whichever of `floor` / `floor + 1` is even; everything else goes to
    // the nearer of the two.
    let round_up = fraction > 0.5 || (fraction == 0.5 && floor % 2.0 != 0.0);
    if round_up {
        floor + 1.0
    } else {
        floor
    }
}

// ---------------------------------------------------------------------------------------------
// §3.5 — the session scale factor
// ---------------------------------------------------------------------------------------------

/// The SES coordinate scale, `SesWriter.java:78-80` / `SesReader.java:73-75`.
///
/// `factor = dsnToBoard(1) / resolution`. Upstream then builds a
/// `CoordinateTransform(factor, 0, 0)` and calls `board_to_dsn`, i.e. **divides**, so a board
/// coordinate `c` is written as `c / factor` and read back as `c * factor`.
///
/// > **Upstream bug, deliberately not reproduced.** `SesReader` also *divides*
/// > (`SesReader.java:295`, `:307`, `:376`), so it is the inverse of the writer only when
/// > `factor == 1` — which is the common case, because §3.2 leaves the board scale factor equal
/// > to the resolution unless the board is big enough to force a reduction. On a board that did
/// > reduce, upstream re-imports its own SES at the square of the intended scale. Both
/// > [`SesCompat`](super::ses_read::SesCompat) modes multiply here; see
/// > [`super::ses_read`] for the divergences that *are* mode-switched.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SesScale {
    factor: f64,
}

impl SesScale {
    /// Builds the scale from the board scale factor (§3.2) and the DSN `(resolution … n)`.
    ///
    /// A non-finite or zero factor — reachable when the §3.2 integer division collapsed the
    /// board scale factor to 0 — falls back to `1.0`. Upstream would produce `Infinity`
    /// coordinates and `Math.round` them to `Long.MAX_VALUE`; a board in that state has already
    /// lost its geometry, and a 1:1 session file is the more useful failure.
    pub fn new(board_scale_factor: f64, resolution: i32) -> SesScale {
        let factor = board_scale_factor / f64::from(resolution);
        SesScale {
            factor: if factor.is_finite() && factor != 0.0 {
                factor
            } else {
                1.0
            },
        }
    }

    /// The raw factor, upstream's `scaleFactor` local.
    pub fn factor(self) -> f64 {
        self.factor
    }

    /// `CoordinateTransform.board_to_dsn` on the session transform.
    pub fn board_to_ses(self, value: f64) -> f64 {
        value / self.factor
    }

    /// The exact inverse. See the type docs for why this is not what upstream's reader does.
    pub fn ses_to_board(self, value: f64) -> f64 {
        value * self.factor
    }
}

// ---------------------------------------------------------------------------------------------
// §12.2 rule 3 — padstack names
// ---------------------------------------------------------------------------------------------

/// A padstack resolved against the board's padstack table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedPadstack {
    /// Index into [`fr_board::PadstackTable`], i.e. what `fr_board::item::Via::padstack_no` holds.
    pub board_padstack_no: u32,
    /// Lowest board layer the padstack covers.
    pub from_layer: i32,
    /// Highest board layer the padstack covers.
    pub to_layer: i32,
    /// The DSN `(attach on|off)` flag of the named padstack.
    pub attach_allowed: bool,
}

/// The name binding between the DSN padstack library and the board's padstack table.
///
/// # Why this is a search and not a lookup
///
/// [`super::build`] interns *placed* pad geometry: one board padstack per
/// `(dsn padstack, pin rotation, component rotation, mirroring, side)` tuple, shared by every
/// item with that placement. A via is always interned at the identity placement
/// (`build.rs`, `insert_wiring_via`), so the board padstack a via points at is byte-identical to
/// the DSN padstack's own geometry — which is what this type matches on. The interning key is
/// private to `build`, and reproducing it here would couple the writer to the importer's cache.
///
/// # Ambiguity
///
/// Two DSN padstacks may lower to identical geometry. For the **write** direction that is
/// harmless: every via padstack is declared in `library_out`, so any geometry-equal via
/// padstack's name is a name KiCad can resolve. [`Self::via_padstack_name`] therefore searches
/// the `library_out` set only, and never returns the name of a pin-only padstack. For the
/// **read** direction the match additionally compares the drill radius, which is derived from
/// the padstack *name* (`fr_rules::Padstack::get_drill_radius`), so distinct names resolve to
/// distinct board padstacks whenever they carry distinct drills.
pub struct SesLibrary {
    /// `library_out` entries: indices into the DSN padstack library, in via-resolution order,
    /// deduplicated by name (`SesWriter.writeLibrary`'s `LinkedHashSet`).
    via_padstacks: Vec<usize>,
    /// Board padstack number → index into `via_padstacks`' backing library, for naming vias.
    board_to_via_padstack: Vec<Option<usize>>,
}

impl SesLibrary {
    /// Binds the library of an imported design to its board.
    pub fn from_design(design: &ImportedDesign) -> SesLibrary {
        SesLibrary::new(
            &design.padstacks,
            &design.via_padstack_indices,
            &design.board,
        )
    }

    /// Binds an explicit padstack library to `board`.
    ///
    /// `via_padstack_indices` is `ImportedDesign::via_padstack_indices` — the padstacks
    /// `(structure (via …))` resolved, in file order.
    pub fn new(
        padstacks: &[ImportedPadstack],
        via_padstack_indices: &[usize],
        board: &Board,
    ) -> SesLibrary {
        // `SesWriter.writeLibrary` deduplicates by name with a `LinkedHashSet`: first
        // occurrence wins, order preserved (§12.2 rule 3, v2-only).
        let mut via_padstacks: Vec<usize> = Vec::with_capacity(via_padstack_indices.len());
        for &index in via_padstack_indices {
            let Some(padstack) = padstacks.get(index) else {
                continue;
            };
            if via_padstacks
                .iter()
                .any(|&seen| padstacks[seen].name == padstack.name)
            {
                continue;
            }
            via_padstacks.push(index);
        }

        let identities: Vec<BoardPadstack> = via_padstacks
            .iter()
            .map(|&index| identity_board_padstack(&padstacks[index]))
            .collect();

        let mut board_to_via_padstack = vec![None; board.padstacks().len()];
        for (board_no, slot) in board_to_via_padstack.iter_mut().enumerate() {
            let Some(board_padstack) = board.padstacks().get(board_no as u32) else {
                continue;
            };
            *slot = identities
                .iter()
                .position(|identity| same_pad_geometry(board_padstack, identity))
                .map(|position| via_padstacks[position]);
        }

        SesLibrary {
            via_padstacks,
            board_to_via_padstack,
        }
    }

    /// The `library_out` entries: indices into the DSN padstack library.
    pub fn via_padstacks(&self) -> &[usize] {
        &self.via_padstacks
    }

    /// The DSN padstack index to write for a via sitting on `board_padstack_no`, or `None` when
    /// no declared via padstack has that geometry.
    ///
    /// `None` is upstream's silent hole: `SesWriter.writeVia` writes
    /// `via.get_padstack().name` unconditionally, and a via whose padstack never reached
    /// `library_out` produces a session file KiCad rejects. Here the caller gets to warn.
    pub fn via_padstack_name<'a>(
        &self,
        padstacks: &'a [ImportedPadstack],
        board_padstack_no: u32,
    ) -> Option<&'a str> {
        let index = *self
            .board_to_via_padstack
            .get(board_padstack_no as usize)?
            .as_ref()?;
        Some(padstacks[index].name.as_str())
    }

    /// `SesReader.processViaScope`'s padstack lookup: strip the `.<digits>` suffix (§14.6), find
    /// the DSN padstack, then find — or create — the board padstack that carries its geometry.
    ///
    /// Creating is the part upstream cannot do: it looks the padstack up in
    /// `board.library.padstacks`, which is populated by the DSN read. Here the board's padstack
    /// table only holds geometry that some item already used, so a session file that introduces
    /// a via padstack no board item used yet needs the entry appended. The appended entry is
    /// exactly what [`super::build`]'s `intern_padstack` would have produced.
    pub fn resolve_or_insert(
        &self,
        padstacks: &[ImportedPadstack],
        name: &str,
        board: &mut Board,
    ) -> Option<ResolvedPadstack> {
        let cleaned = strip_numeric_suffix(name);
        let padstack = padstacks.iter().find(|p| p.name == cleaned)?;
        let identity = identity_board_padstack(padstack);
        let existing = (0..board.padstacks().len()).find(|&board_no| {
            board
                .padstacks()
                .get(board_no as u32)
                .is_some_and(|candidate| *candidate == identity)
        });
        let board_padstack_no = match existing {
            Some(board_no) => board_no as u32,
            None => board.padstacks_mut().push(identity),
        };
        Some(ResolvedPadstack {
            board_padstack_no,
            from_layer: padstack.from_layer() as i32,
            to_layer: padstack.to_layer() as i32,
            attach_allowed: padstack.attach_allowed,
        })
    }
}

/// The board padstack [`super::build`]'s `intern_padstack` produces for the identity placement:
/// front side, no pin rotation, no component rotation, no mirroring, no translation.
///
/// `placed_pad_shapes` at the identity reduces to a copy — `turn_90_degree` with factor 0 is
/// skipped, `translate_by((0,0))` is the identity, and neither flip applies — so the shapes are
/// the DSN library's own, lowered through [`PadGeometry::to_tile_shape`].
fn identity_board_padstack(padstack: &ImportedPadstack) -> BoardPadstack {
    let from_layer = padstack.from_layer();
    let to_layer = padstack.to_layer();
    let mut shapes: Vec<Option<TileShape>> = Vec::with_capacity(to_layer - from_layer + 1);
    for layer in from_layer..=to_layer {
        shapes.push(
            padstack
                .shapes
                .get(layer)
                .and_then(Option::as_ref)
                .map(PadGeometry::to_tile_shape),
        );
    }
    BoardPadstack {
        from_layer: from_layer as i32,
        to_layer: to_layer as i32,
        shapes,
        drill_radius: drill_radius(padstack),
    }
}

/// `ImportedPadstack::to_rules_padstack(..).get_drill_radius().round()`, which is what
/// `intern_padstack` stores. Both that lowering and `PadGeometry::to_pad_shape` are private to
/// [`super::build`]; they are repeated here because the alternative is widening `build`'s API
/// for one caller. `padstack_geometry_matches_the_importers_interning` guards the copy.
fn drill_radius(padstack: &ImportedPadstack) -> i32 {
    let mut shapes: Vec<Option<PadShape>> = Vec::with_capacity(padstack.shapes.len());
    for slot in &padstack.shapes {
        shapes.push(slot.as_ref().map(to_pad_shape));
    }
    RulesPadstack::new(
        padstack.name.clone(),
        0,
        shapes,
        padstack.attach_allowed,
        padstack.placed_absolute,
    )
    .get_drill_radius()
    .round() as i32
}

/// `PadGeometry::to_pad_shape`, which `build` keeps private.
fn to_pad_shape(geometry: &PadGeometry) -> PadShape {
    match geometry {
        PadGeometry::Box(b) => PadShape::Box(*b),
        PadGeometry::Octagon(o) => PadShape::Octagon(*o),
        PadGeometry::Circle(c) => PadShape::Circle { radius: c.radius },
        PadGeometry::Simplex(s) => PadShape::Octagon(
            s.bounding_octagon()
                .unwrap_or_else(|| s.bounding_box().to_int_octagon()),
        ),
    }
}

/// Layer span and pad shapes only — the fields the placement determines.
///
/// `drill_radius` is deliberately excluded: it is derived from the padstack *name*, so two DSN
/// padstacks that share geometry but not a name would fail an equality test that included it,
/// and the write direction only needs a name KiCad can resolve.
fn same_pad_geometry(left: &BoardPadstack, right: &BoardPadstack) -> bool {
    left.from_layer == right.from_layer
        && left.to_layer == right.to_layer
        && left.shapes == right.shapes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_rint_breaks_ties_to_even() {
        assert_eq!(java_rint(0.5), 0.0);
        assert_eq!(java_rint(1.5), 2.0);
        assert_eq!(java_rint(2.5), 2.0);
        assert_eq!(java_rint(-0.5), -0.0);
        assert_eq!(java_rint(-1.5), -2.0);
        assert_eq!(java_rint(339.4), 339.0);
    }

    #[test]
    fn session_scale_is_one_when_the_board_factor_survived_unreduced() {
        // §3.2: an unreduced board keeps `scaleFactor == resolution`, so SES coordinates are
        // board coordinates. Issue026 is the worked example (`transform.rs` tests).
        let scale = SesScale::new(10.0, 10);
        assert_eq!(scale.factor(), 1.0);
        assert_eq!(scale.board_to_ses(1_409_750.0), 1_409_750.0);
        assert_eq!(scale.ses_to_board(1_409_750.0), 1_409_750.0);
    }

    #[test]
    fn session_scale_round_trips_a_reduced_board_factor() {
        // resolution 100 reduced to a board factor of 1 (`transform.rs`,
        // `large_board_reduces_the_factor_by_integer_division`): SES coordinates are 100x the
        // board's, and reading multiplies back.
        let scale = SesScale::new(1.0, 100);
        assert_eq!(scale.board_to_ses(5.0), 500.0);
        assert_eq!(scale.ses_to_board(500.0), 5.0);
    }

    #[test]
    fn a_collapsed_board_scale_factor_falls_back_to_one_to_one() {
        // §3.2's integer division can reach 0; upstream would then divide by zero.
        let scale = SesScale::new(0.0, 10);
        assert_eq!(scale.factor(), 1.0);
    }
}
