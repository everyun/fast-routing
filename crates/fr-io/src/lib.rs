//! Specctra DSN / SES file format I/O for the Freerouting Rust port.
//!
//! Mirrors `app.freerouting.io.specctra` from the upstream Java project:
//!
//! - `DsnReader` / `parse_dsn` — recursive-descent Specctra DSN parser.
//! - `SesWriter` — Specctra SES session file generator.
//! - `DsnDocument` — complete structured AST of a PCB design.

pub mod dsn_reader;
pub mod keyword;
pub mod lexer;
pub mod parser;
pub mod ses_writer;

pub use dsn_reader::{parse_dsn, DsnReader};
pub use keyword::Keyword;
pub use lexer::{DsnLexer, Token};
pub use parser::{
    DsnClass, DsnComponent, DsnDocument, DsnLayer, DsnNet, DsnNetPin, DsnPackage, DsnPadstack,
    DsnPadstackShape, DsnPin, DsnPoint, DsnVia, DsnWire,
};
pub use ses_writer::SesWriter;
