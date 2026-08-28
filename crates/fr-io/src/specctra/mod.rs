//! The Specctra DSN importer, rebuilt against `docs/upstream-study/dsn-board-semantics.md`.
//!
//! This is the M1 item 6 read path: DSN bytes → [`fr_board::Board`] + [`fr_rules::BoardRules`].
//! It lives beside the pre-rewrite [`crate::dsn_reader`], which stays until `fr-core` moves over.
//!
//! ```no_run
//! let bytes = std::fs::read("board.dsn").unwrap();
//! let design = fr_io::specctra::import(&bytes).unwrap();
//! println!("{} layers, scale {}", design.board.layers().len(), design.scale_factor);
//! ```
//!
//! # Layout
//!
//! | module | upstream counterpart |
//! |---|---|
//! | [`lex`] | `SpecctraDsnStreamReader` — the JFlex DFA *and* the `nextString` raw-buffer bypass |
//! | [`read`] | the 36 `readScope` methods under `io/specctra/parser/` |
//! | [`ast`] | the shape/rule/package/placement records those readers fill, plus the geometry conversion |
//! | [`transform`] | `io/CoordinateTransform` and `Structure.createBoard`'s scale-factor loop |
//! | [`jround`] | `java.lang.Math.round` — the half-up rounding every `transformToBoard*` ends in |
//! | [`build`] | `Structure.createBoard`, `Library.readScope`'s tail, `Network.insertComponent`, `Wiring`'s inserts, and the two `HeadlessBoardManager` post-load overrides |
//!
//! The read/build split is the one structural change `dsn-board-semantics.md` §17.4 asks for:
//! upstream constructs the board halfway through the structure scope and inserts items halfway
//! through the network scope, which is exactly why its cross-scope state bag exists.
//!
//! # What is deliberately not reproduced
//!
//! Recorded in `docs/PARITY-DIVERGENCES.md`; the one that changes board contents is
//! `Wiring.readViaScope`'s missing `currentIndex++`, where we follow v1.9 and give a
//! multi-subnet via its full net array.
//!
//! # Status
//!
//! Upstream quirks reproduced on purpose, each asserted by a test: the `autoroute_settings`
//! `layerStructure == null` guard (§6.6), the `class_class` shared-iterator pairing (§7.7), the
//! `separateHoles` last-writer-wins assignment (§6.3), the integer-division scale-factor
//! reduction (§3.2), the two different trace-half-width roundings (§3.4), `(type route)` →
//! `USER_FIXED` (§10.3), and `NetClass.readScope`'s overshoot on a sub-scope-less `(class …)`.
//!
//! Known gaps, in the order they are likely to matter to the golden comparator:
//!
//! | gap | effect |
//! |---|---|
//! | `RoutingBoard.reduceNetsOfRouteItems` — the post-load pass that trims a multi-net trace or via down to the nets its contacts actually carry — is **not** run. It is an `fr-board` method upstream (`RoutingBoard.java:1284`), called by the manager after load, and it needs an in-place net-array mutation the `Board` API does not expose; doing it here through remove + re-insert would renumber items. Call it from [`build`] once `fr-board` grows it. | multi-subnet wiring items keep every subnet of their net name, which is also where the `readViaScope` divergence shows up |
//! | Areas with `(window …)` holes keep only the border; the holes are dropped with a warning. `fr-geometry` has no area-with-holes type and no polygon `split_to_convex` yet (`fr-board/search_tree.rs` §3.5 note). | keepout/pour geometry is *larger* than upstream's, never smaller |
//! | A circular keepout or pour is stored as its bounding octagon, because `fr_board::AreaShape` has no circle variant. | slightly larger obstacle |
//! | `Wiring.tryCorrectNet` (adopt a contact's net when a wire imported with none) is not implemented. | Mentor "turret" traces stay net-less |
//! | `(part_library …)` logical parts / gate-and-pin swap are parsed as an unknown scope and skipped. | interactive-only upstream; `Network.insertLogicalParts`'s return value is ignored there too |
//! | A structure keepout on an unknown layer warns and is skipped instead of aborting the whole structure scope. | more boards load |
//! | A package pin whose padstack is missing is dropped with a warning instead of aborting the whole library read. | the rest of the library survives |
//! | The `hole_edge` clearance class and the circle-keepout reclassification are only reached when a caller raises `hole_clearance_um` above the shipped default of 0. | none at the default |
//! | Board padstacks lower a circular pad to its bounding octagon and a simplex via pad to its bounding octagon, because neither `fr_board::TileShape` nor `fr_rules::PadShape` spans upstream's four `ConvexShape` implementations. [`ImportedDesign::padstacks`] keeps the exact geometry. | DRC-visible on round pads |
//!
//! # Deliberate divergences from the shipped scanner
//!
//! `docs/PARITY-DIVERGENCES.md` is the register; this one is here because it is a [`lex`]
//! decision and it is what the golden gate reports on `Issue110-RelayModule`.
//!
//! **Non-ANSI characters in an unquoted identifier survive here and do not survive upstream.**
//! `lex.rs`'s `SpecCharANSI` is taken from the checked-in `SpecctraFileDescription.flex`, which
//! predates the shipped scanner. The generated `SpecctraDsnStreamReader` maps every code point
//! above `U+00FF` to character class 0, whose action (`case 2`, `:1277`) is
//! `FRLogger.warn("Non-ansi character '…' found at position …")` **and then `break`** — the
//! character is dropped and scanning continues in the same token. Quoted strings are unaffected,
//! because `STRING1` appends `yytext()` verbatim. So on `Issue110-RelayModule` the release jar
//! reads `(component Соединители:PinSocket_1x08_P2.54mm_Horizontal` as
//! `:PinSocket_1x08_P2.54mm_Horizontal` and `(image Предохранители:Цилиндр_5х20` as `:`, and its
//! dump says so; the same file's *quoted* `"Соединители:KF301-2P"` round-trips intact.
//!
//! We do not reproduce it. The mangling is silent data loss on exactly the boards freerouting
//! issue #110 is about, it is self-consistent on both sides (definition and reference are mangled
//! identically, so the board still resolves either way), and nothing downstream reads an image
//! name except the placement lookup. The cost is that the `components` and `images` groups of
//! that one fixture can never go green against this oracle: the values agree field for field, only
//! the names differ.

pub mod ast;
pub mod build;
pub mod jround;
pub mod lex;
pub mod read;
pub mod ses_common;
pub mod ses_read;
pub mod ses_write;
pub mod transform;

pub use ast::{DsnAst, Unit};
pub use build::{
    ImportOptions, ImportedComponent, ImportedDesign, ImportedPackage, ImportedPadstack,
    PadGeometry,
};
pub use jround::{jround, JavaRound};
pub use lex::{Kw, LexState, Lexer, Token};
pub use read::{ReadOutcome, Reader};
pub use ses_common::{java_rint, ResolvedPadstack, SesLibrary, SesScale};
pub use ses_read::{
    read_session, read_session_with_options, SesCompat, SesImportSummary, SesReadError,
    SesReadOptions, SesTarget,
};
pub use ses_write::{write_session, write_session_verbose, SesInput, SesOutput};
pub use transform::{derive_scale_factor, CoordinateTransform, ScaleFactor};

/// `DsnFile.CLASS_CLEARANCE_SEPARATOR` — the character `(type …)` lists are split on. `-`, not
/// `_`, which is why `(type smd-via)` and `(type smd_via)` take different branches downstream.
pub const CLASS_CLEARANCE_SEPARATOR_BYTE: u8 = b'-';

/// Why an import failed.
#[derive(Debug, Clone, PartialEq)]
pub enum ImportError {
    /// The header was not `(pcb <name>`.
    NotADsnFile,
    /// A scope reader gave up.
    ParseError(String),
    /// The structure scope produced no usable board — upstream's
    /// `BoardReadResult.OutlineMissing` and the `layerCount == 0` abort both land here.
    BoardConstruction(String),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportError::NotADsnFile => {
                write!(
                    f,
                    "not a Specctra DSN file: expected a '(pcb <name>' header"
                )
            }
            ImportError::ParseError(message) => write!(f, "DSN parse error: {message}"),
            ImportError::BoardConstruction(message) => {
                write!(f, "DSN board construction: {message}")
            }
        }
    }
}

impl std::error::Error for ImportError {}

/// Parses and builds a DSN file with the shipped post-load overrides.
pub fn import(src: &[u8]) -> Result<ImportedDesign, ImportError> {
    import_with_options(src, ImportOptions::default())
}

/// Parses and builds a DSN file.
pub fn import_with_options(
    src: &[u8],
    options: ImportOptions,
) -> Result<ImportedDesign, ImportError> {
    let (outcome, warnings) = Reader::read(src);
    let ast = match outcome {
        ReadOutcome::Ok(ast) => ast,
        ReadOutcome::NotADsnFile => return Err(ImportError::NotADsnFile),
        ReadOutcome::ParseError(message) => return Err(ImportError::ParseError(message)),
    };
    build::build(&ast, warnings, options).map_err(ImportError::BoardConstruction)
}

/// Parses a DSN file without building a board — used by tests that assert on the AST and by the
/// metadata fast path.
pub fn parse(src: &[u8]) -> (ReadOutcome, Vec<String>) {
    Reader::read(src)
}

/// Java `name.replaceAll("\\.\\d+", "")`: removes **every** `.` followed by one or more digits,
/// anywhere in the string.
///
/// KiCad appends `.1`, `.2`, … to padstacks that differ only in a property Freerouting ignores,
/// and all five lookup sites strip it (`dsn-board-semantics.md` §14.6). The regex is not anchored,
/// so an Eagle padstack called `Round1$13.779528` becomes `Round1$13` — at the definition site
/// *and* at every lookup site, so the two still meet.
pub fn strip_numeric_suffix(name: &str) -> String {
    let bytes = name.as_bytes();
    let mut result = String::with_capacity(name.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'.' {
            let mut end = index + 1;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            if end > index + 1 {
                index = end;
                continue;
            }
        }
        // Push whole UTF-8 sequences so multi-byte characters survive.
        let start = index;
        index += 1;
        while index < bytes.len() && (bytes[index] & 0xC0) == 0x80 {
            index += 1;
        }
        result.push_str(&name[start..index]);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_suffix_stripping_matches_the_java_regex() {
        assert_eq!(
            strip_numeric_suffix("Round[A]Pad_600_um.1"),
            "Round[A]Pad_600_um"
        );
        assert_eq!(strip_numeric_suffix("Round1$13.779528"), "Round1$13");
        assert_eq!(strip_numeric_suffix("a.1b.22c"), "abc");
        // A dot not followed by a digit stays.
        assert_eq!(strip_numeric_suffix("B.Cu"), "B.Cu");
        assert_eq!(strip_numeric_suffix("Паяльная.3"), "Паяльная");
    }
}
