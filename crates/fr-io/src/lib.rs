//! Specctra DSN / SES file format I/O for the Freerouting Rust port.
//!
//! Mirrors `app.freerouting.io.specctra` from the upstream Java project:
//!
//! - `DsnReader` / `parse_dsn` — recursive-descent Specctra DSN parser.
//! - `SesWriter` — Specctra SES session file generator.
//! - `DsnDocument` — complete structured AST of a PCB design.
//!
//! # Two read paths
//!
//! [`specctra`] is the real importer: a faithful port of `io/specctra/**` that produces an
//! `fr_board::Board` plus `fr_rules::BoardRules`, following
//! `docs/upstream-study/dsn-board-semantics.md`. It is what M1 item 6 and gate G1.1 are about,
//! and it is where new work goes.
//!
//! [`dsn_reader`] and the [`lexer`] / [`parser`] / [`keyword`] modules under it are the
//! pre-rewrite path, kept only until `fr-core` moves over. They parse into a loose
//! [`DsnDocument`] and are **not** conformant — their tokenizer loses numeric identifiers, among
//! other things. No new code should call [`parse_dsn`].
//!
//! [`golden_dump`] serialises an [`specctra::ImportedDesign`] into the
//! `fast-routing.golden-dump/1` exchange document that `bench/tools/golden_compare.py` diffs
//! against the release jar's own dump — the G1.1 import-parity gate.

pub mod dsn_reader;
pub mod golden_dump;
pub mod keyword;
pub mod lexer;
pub mod parser;
pub mod ses_writer;
pub mod specctra;

pub use dsn_reader::{parse_dsn, DsnReader};
pub use keyword::Keyword;
pub use lexer::{DsnLexer, Token};
pub use parser::{
    DsnClass, DsnComponent, DsnDocument, DsnLayer, DsnNet, DsnNetPin, DsnPackage, DsnPadstack,
    DsnPadstackShape, DsnPin, DsnPoint, DsnVia, DsnWire,
};
pub use ses_writer::SesWriter;
