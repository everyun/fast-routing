//! `io/specctra/SesWriter.java` — the Specctra session (`.ses`) writer
//! (`dsn-board-semantics.md` §12).
//!
//! ```no_run
//! use fr_io::specctra::ses_write::{write_session, SesInput};
//!
//! let bytes = std::fs::read("board.dsn").unwrap();
//! let design = fr_io::specctra::import(&bytes).unwrap();
//! std::fs::write("board.ses", write_session(&SesInput::from_design(&design, "board.dsn"))).unwrap();
//! ```
//!
//! # What the format demands
//!
//! KiCad's `SPECCTRA_DB::LoadSESSION` reads `(routes (network_out (net … (wire (path …)) (via …))))`
//! plus `(placement …)`, and it is unforgiving about four things (§12.2):
//!
//! 1. **Coordinates and widths are integers, except exact half-unit corners** — upstream rounds
//!    everything through `(int) Math.round` ([`jround`], not Rust's `f64::round`); this writer
//!    keeps that for everything that fits, and writes a pull-tight `.5` corner as one decimal
//!    so the round trip cannot move copper into a clearance-exact gap (see
//!    [`SesFileWriter::coordinate_text`]).
//! 2. **Layer names inside `(path …)` are the DSN's**, byte for byte.
//! 3. **Via padstacks are declared in `library_out` before use**, by name, deduplicated.
//! 4. **Trace endpoints must land on pad centres** where they legitimately can, or KiCad reports
//!    the net unconnected. That is [`SesFileWriter::snapped_endpoint`], §12.3, and it is v2-only.
//!
//! On top of those, the identifier quoting set is wider than the DSN writer's
//! ([`SES_RESERVED_CHARS`]) and `SYSTEM_FIXED` items are omitted, which is how DSN-imported
//! keepouts and locked component copper stay out of the session.
//!
//! # Wired into `fr-cli`
//!
//! `fr-core`'s pipeline drives this module (`write_session_verbose`); the
//! pre-rewrite [`crate::ses_writer`] remains for older callers only.

use std::fmt::Write as _;

use fr_board::{AreaShape, FixedState, ItemId, ItemKind, TileShape};
use fr_geometry::planar::{FloatPoint, IntBox, IntVector, Shape as _, Vector};
use fr_rules::Nets;

use super::ast::Unit;
use super::build::{
    ImportedComponent, ImportedDesign, ImportedPackage, ImportedPadstack, PadGeometry,
};
use super::jround::jround;
use super::ses_common::{java_rint, SesLibrary, SesScale};

/// `SesWriter.java:62` — the characters that force an identifier into quotes.
///
/// Strictly more than the DSN writer's `{ ( ) SPACE ; - _ }` and far more than v1.9's
/// `{ ( ) SPACE - }`. The four v2 additions `/ ~ { }` are what make KiCad net names like `/CLK`,
/// `~{RESET}` and hierarchical sheet paths survive a round trip.
pub const SES_RESERVED_CHARS: [char; 10] = ['(', ')', ' ', ';', '-', '_', '/', '~', '{', '}'];

// =============================================================================================
// Input
// =============================================================================================

/// Everything the writer needs that a [`fr_board::Board`] does not carry.
///
/// The board holds geometry, nets-as-numbers and layer names; net *names*, padstack *names*,
/// component placements and the parser echo all live in the importer's output. Keeping them in
/// one borrowed struct is what lets a future router-produced board be written without going
/// back through [`ImportedDesign`].
pub struct SesInput<'a> {
    /// `(base_design …)`, and — with `.dsn` replaced by `.ses` — the session name.
    pub design_name: &'a str,
    /// `(resolution <unit> …)`.
    pub unit: Unit,
    /// `(resolution … <n>)`.
    pub resolution: i32,
    /// The §3.2 board scale factor, i.e. `dsnToBoard(1)`.
    pub board_scale_factor: f64,
    /// The board whose routing is serialised.
    pub board: &'a fr_board::Board,
    /// Net numbers → names.
    pub nets: &'a Nets,
    /// Placed components, in file order. Component `i` owns the board items with
    /// `component_no == i + 1` ([`super::build`]'s `next_component_no` starts at 1 and
    /// increments once per pushed component).
    pub components: &'a [ImportedComponent],
    /// The package library; `(component <packageName> …)` groups by it.
    pub packages: &'a [ImportedPackage],
    /// The padstack library, for `library_out` names and shapes.
    pub padstacks: &'a [ImportedPadstack],
    /// The name binding between `padstacks` and `board`'s padstack table.
    pub library: SesLibrary,
    /// `(string_quote …)` echoed from the input DSN — the character identifiers are quoted with.
    pub string_quote: &'a str,
    /// `(host_cad …)`, echoed into the reduced `(parser …)` scope.
    pub host_cad: Option<&'a str>,
    /// `(host_version …)`, likewise.
    pub host_version: Option<&'a str>,
    /// `(constant a b)` pairs, likewise. [`ImportedDesign`] does not keep them, so
    /// [`SesInput::from_design`] leaves this empty; a caller holding the AST can supply them.
    pub constants: Vec<(String, String)>,
    /// `(write_resolution <char> <n>)`. Never parsed by [`super::read`], so always `None` here.
    pub write_resolution: Option<(char, i32)>,
}

impl<'a> SesInput<'a> {
    /// Binds a freshly imported design for writing.
    ///
    /// `design_name` is the `(base_design …)` token *and* the seed for the session name.
    /// Upstream passes the routing job's file name here, which is why it may differ from
    /// [`ImportedDesign::design_name`] (the `(pcb <name>)` token); pass `&design.design_name`
    /// to use the latter.
    pub fn from_design(design: &'a ImportedDesign, design_name: &'a str) -> SesInput<'a> {
        SesInput {
            design_name,
            unit: design.unit,
            resolution: design.resolution,
            board_scale_factor: design.scale_factor,
            board: &design.board,
            nets: &design.rules.nets,
            components: &design.components,
            packages: &design.packages,
            padstacks: &design.padstacks,
            library: SesLibrary::from_design(design),
            string_quote: &design.string_quote,
            host_cad: design.host_cad.as_deref(),
            host_version: design.host_version.as_deref(),
            constants: Vec::new(),
            write_resolution: None,
        }
    }
}

/// The session file plus whatever the writer wanted to complain about.
pub struct SesOutput {
    /// The `.ses` text. UTF-8, ends with `\n)` and **no** trailing newline, like upstream's
    /// `IndentFileWriter`.
    pub text: String,
    /// The `FRLogger.warn` calls upstream drops on the floor.
    pub warnings: Vec<String>,
}

/// `SesWriter.write(board, out, designName)` — the whole session file as text.
pub fn write_session(input: &SesInput<'_>) -> String {
    write_session_verbose(input).text
}

/// As [`write_session`], but also returning the warnings upstream only logs.
pub fn write_session_verbose(input: &SesInput<'_>) -> SesOutput {
    let mut writer = SesFileWriter {
        out: IndentWriter::new(),
        scale: SesScale::new(input.board_scale_factor, input.resolution),
        input,
        warnings: Vec::new(),
        contacts: Vec::new(),
    };
    writer.write_session_scope();
    SesOutput {
        text: writer.out.out,
        warnings: writer.warnings,
    }
}

// =============================================================================================
// datastructures/IndentFileWriter
// =============================================================================================

/// `datastructures/IndentFileWriter`: two spaces per nesting level, `\n` before every scope
/// boundary.
///
/// `start_scope(true)` emits the newline first, `start_scope(false)` does not — the session
/// scope is the only caller of the latter, which is why the file starts with `(` and not `\n(`.
struct IndentWriter {
    out: String,
    level: usize,
}

impl IndentWriter {
    fn new() -> IndentWriter {
        IndentWriter {
            out: String::new(),
            level: 0,
        }
    }

    fn write(&mut self, text: &str) {
        self.out.push_str(text);
    }

    /// `String.valueOf(int)`. Formats straight into the buffer — a `to_string()` per coordinate
    /// would allocate once per corner of every trace.
    fn write_int(&mut self, value: i64) {
        let _ = write!(self.out, "{value}");
    }

    fn new_line(&mut self) {
        self.out.push('\n');
        for _ in 0..self.level {
            self.out.push_str("  ");
        }
    }

    fn start_scope(&mut self) {
        self.start_scope_opt(true);
    }

    fn start_scope_opt(&mut self, new_line: bool) {
        if new_line {
            self.new_line();
        }
        self.out.push('(');
        self.level += 1;
    }

    fn end_scope(&mut self) {
        self.level -= 1;
        self.new_line();
        self.out.push(')');
    }

    /// `IdentifierType.write` with the SES reserved set (see [`needs_quotes`]).
    ///
    /// > **Not reproduced:** upstream's leading quote-strip loop is
    /// > `p_name.substring(1, p_name.length() - 2)`, which drops the closing quote *and the last
    /// > real character* (`"abc"` → `ab`). Nothing reaches it in this port — [`super::lex`]
    /// > already unquotes every identifier — and reproducing a truncation bug would corrupt
    /// > names the importer got right.
    fn write_identifier(&mut self, name: &str, string_quote: &str) {
        // "if the name contains our quote character, we must remove it".
        let cleaned;
        let name = if !string_quote.is_empty() && name.contains(string_quote) {
            cleaned = name.replace(string_quote, "");
            cleaned.as_str()
        } else {
            name
        };
        if needs_quotes(name) {
            self.out.push_str(string_quote);
            self.out.push_str(name);
            self.out.push_str(string_quote);
        } else {
            self.out.push_str(name);
        }
    }
}

/// The `need_quotes` triggers, in upstream's order, plus one deliberate deviation.
fn needs_quotes(name: &str) -> bool {
    // Deviation: upstream writes an empty name as zero bytes, producing `(component ` with no
    // name — a session KiCad's specctra parser rejects outright (observed on
    // cla45-compute-hdi6cs, whose DSN declares `(image ""`). Quoting yields `(component "")`.
    if name.is_empty() {
        return true;
    }
    if name.chars().any(|c| SES_RESERVED_CHARS.contains(&c)) {
        return true;
    }
    // `for (byte ch : name.getBytes(UTF_8)) if (ch <= 0)`: every byte of a multi-byte UTF-8
    // sequence has its high bit set, so a signed `byte` is negative — i.e. "non-ASCII". A NUL
    // byte trips the same test.
    if name.bytes().any(|b| b >= 0x80 || b == 0) {
        return true;
    }
    // `name.matches("^-?\\d.*")`. The `-?` is dead weight next to the reserved set, which
    // already contains `-`; what this rule really catches is a name starting with a digit.
    let mut chars = name.chars();
    match chars.next() {
        Some('-') => chars.next().is_some_and(|c| c.is_ascii_digit()),
        Some(c) => c.is_ascii_digit(),
        None => false,
    }
}

// =============================================================================================
// The writer proper
// =============================================================================================

struct SesFileWriter<'a, 'b> {
    out: IndentWriter,
    scale: SesScale,
    input: &'a SesInput<'b>,
    warnings: Vec<String>,
    /// Reused across [`SesFileWriter::snapped_endpoint`] calls — a fresh vector per trace
    /// endpoint would allocate twice per wire.
    contacts: Vec<ItemId>,
}

impl SesFileWriter<'_, '_> {
    fn warn(&mut self, message: impl Into<String>) {
        self.warnings.push(message.into());
    }

    /// Rounds a board length into the session's integer grid.
    fn coordinate(&self, board_value: f64) -> i64 {
        jround(self.scale.board_to_ses(board_value))
    }

    /// Session-coordinate text for an exact board-space value.
    ///
    /// Integers write as integers — the upstream bytes. A value exactly on the
    /// half grid writes with one decimal: `Polyline` corners can sit between
    /// lattice points (diagonal∩diagonal intersections survive pull-tight),
    /// and `(int) Math.round` — what upstream writes, [`Self::coordinate`] —
    /// moves them by up to 0.5 board units, which turned a clearance-exact
    /// gap into a re-import violation on Issue102-Mars-64-revE-rot00. The
    /// reader reconstructs the exact polyline from the decimal
    /// (`Polyline::from_exact_corners`); upstream's reader rounds it the same
    /// way it rounds its own sessions, so the file stays compatible.
    ///
    /// On a board whose session scale factor is not 1 the half unit is not
    /// the kernel's representability boundary anyway, and for fractions finer
    /// than a half the kernel cannot hold the line at all — both keep the
    /// legacy rounded integer.
    fn coordinate_text(&self, ses_value: f64) -> String {
        let rounded = jround(ses_value);
        if (ses_value - rounded as f64).abs() < 1e-6 {
            return rounded.to_string();
        }
        let s = format!("{ses_value:.4}");
        let trimmed = s.trim_end_matches('0').trim_end_matches('.');
        trimmed.to_string()
    }

    fn identifier(&mut self, name: &str) {
        let quote = self.input.string_quote;
        self.out.write_identifier(name, quote);
    }

    // -----------------------------------------------------------------------------------------
    // §12.1 — the scope tree
    // -----------------------------------------------------------------------------------------

    fn write_session_scope(&mut self) {
        // `designName.replace(".dsn", ".ses")` — a plain substring replace, so `a.dsn.b.dsn`
        // becomes `a.ses.b.ses`. Kept verbatim: the session name is cosmetic, but a golden
        // comparison against the jar is not.
        let design_name = self.input.design_name;
        let session_name = design_name.replace(".dsn", ".ses");
        self.out.start_scope_opt(false);
        self.out.write("session ");
        self.identifier(&session_name);
        self.out.new_line();
        self.out.write("(base_design ");
        self.identifier(design_name);
        self.out.write(")");
        self.write_placement();
        self.write_was_is();
        self.write_routes();
        self.out.end_scope();
    }

    fn write_resolution_scope(&mut self) {
        self.out.new_line();
        self.out.write("(resolution ");
        self.out.write(self.input.unit.as_str());
        self.out.write(" ");
        self.out.write_int(i64::from(self.input.resolution));
        self.out.write(")");
    }

    // -----------------------------------------------------------------------------------------
    // placement
    // -----------------------------------------------------------------------------------------

    fn write_placement(&mut self) {
        let input = self.input;
        self.out.start_scope();
        self.out.write("placement");
        self.write_resolution_scope();

        // `SesWriter.writeComponents` re-scans every board item per component to prove the
        // component still has copper ("check that not all items of the component are deleted").
        // One pass over the store answers it for all of them.
        let mut alive = vec![false; input.components.len() + 1];
        for (_, item) in input.board.store().iter() {
            let component_no = item.component_no;
            if component_no > 0 && (component_no as usize) < alive.len() {
                alive[component_no as usize] = true;
            }
        }

        for package_index in 0..input.packages.len() {
            self.write_components(package_index, &alive);
        }

        self.out.end_scope();
    }

    fn write_components(&mut self, package_index: usize, alive: &[bool]) {
        let input = self.input;
        let mut component_found = false;
        for (index, component) in input.components.iter().enumerate() {
            if component.package_index != package_index {
                continue;
            }
            // `build`'s component numbering: file order plus one.
            if !alive[index + 1] {
                continue;
            }
            let Some(location) = component.location else {
                // Unreachable — an unplaced component contributes no board items, so `alive`
                // already filtered it. Upstream would have thrown a NullPointerException here.
                continue;
            };
            if !component_found {
                self.out.start_scope();
                self.out.write("component ");
                self.identifier(&input.packages[package_index].name);
                component_found = true;
            }
            let x = self.coordinate(f64::from(location.x));
            let y = self.coordinate(f64::from(location.y));

            self.out.new_line();
            self.out.write("(place ");
            self.identifier(&component.name);
            self.out.write(" ");
            self.out.write_int(x);
            self.out.write(" ");
            self.out.write_int(y);
            self.out.write(if component.is_front {
                " front "
            } else {
                " back "
            });
            self.out
                .write(&format_placement_rotation(component.rotation));
            if component.position_fixed {
                self.out.new_line();
                self.out.write(" (lock_type position)");
            }
            self.out.write(")");
        }
        if component_found {
            self.out.end_scope();
        }
    }

    /// `SesWriter.writeWasIs` — always an empty scope here.
    ///
    /// Upstream emits `(pins A-1 B-2)` for every pin whose `getChangedTo()` differs from itself,
    /// which only the interactive pin-and-gate-swap commands ever set. This port has no pin
    /// swapping (§9.6: `(part_library …)` is parsed and discarded), so the scope is empty —
    /// which is also what upstream writes for every non-interactive run.
    fn write_was_is(&mut self) {
        self.out.start_scope();
        self.out.write("was_is");
        self.out.end_scope();
    }

    // -----------------------------------------------------------------------------------------
    // routes
    // -----------------------------------------------------------------------------------------

    fn write_routes(&mut self) {
        self.out.start_scope();
        self.out.write("routes ");
        self.write_resolution_scope();
        self.write_parser_scope();
        self.write_library_out();
        self.write_network_out();
        self.out.end_scope();
    }

    /// `Parser.write_scope(.., reduced = true)` (§12.2 rule 5): no `(string_quote …)`, no
    /// `(space_in_quoted_tokens on)`, no `(generated_by_freeroute)`.
    fn write_parser_scope(&mut self) {
        let input = self.input;
        self.out.start_scope();
        self.out.write("parser");
        if let Some(host_cad) = input.host_cad {
            self.out.new_line();
            self.out.write("(host_cad ");
            self.identifier(host_cad);
            self.out.write(")");
        }
        if let Some(host_version) = input.host_version {
            self.out.new_line();
            self.out.write("(host_version ");
            self.identifier(host_version);
            self.out.write(")");
        }
        for (first, second) in &input.constants {
            self.out.new_line();
            self.out.write("(constant ");
            // Upstream writes a trailing space after *every* element, the last one included.
            self.identifier(first);
            self.out.write(" ");
            self.identifier(second);
            self.out.write(" ");
            self.out.write(")");
        }
        if let Some((char_name, positive_int)) = input.write_resolution {
            self.out.new_line();
            self.out.write("(write_resolution ");
            self.out.write(char_name.encode_utf8(&mut [0u8; 4]));
            self.out.write(" ");
            self.out.write_int(i64::from(positive_int));
            self.out.write(")");
        }
        self.out.end_scope();
    }

    /// `SesWriter.writeLibrary` (§12.2 rule 3): via padstacks only, deduplicated by name.
    fn write_library_out(&mut self) {
        let input = self.input;
        self.out.start_scope();
        self.out.write("library_out ");
        for &padstack_index in input.library.via_padstacks() {
            self.write_padstack(padstack_index);
        }
        self.out.end_scope();
    }

    fn write_padstack(&mut self, padstack_index: usize) {
        let input = self.input;
        let padstack = &input.padstacks[padstack_index];
        let layer_count = input.board.layers().len();
        let first_layer = padstack
            .shapes
            .iter()
            .take(layer_count)
            .position(Option::is_some);
        let last_layer = padstack
            .shapes
            .iter()
            .take(layer_count)
            .rposition(Option::is_some);
        let (Some(first_layer), Some(last_layer)) = (first_layer, last_layer) else {
            self.warn(format!(
                "SesWriter.writePadstack: padstack shape not found for '{}'",
                padstack.name
            ));
            return;
        };

        self.out.start_scope();
        self.out.write("padstack ");
        self.identifier(&padstack.name);
        for layer in first_layer..=last_layer {
            let Some(geometry) = padstack.shapes[layer].as_ref() else {
                continue;
            };
            self.out.start_scope();
            self.out.write("shape");
            self.write_shape_int(geometry, layer as i32);
            self.out.end_scope();
        }
        // §12.2 rule 4: `(attach off)` only when the padstack forbids attachment;
        // `(absolute on)` is DSN-only.
        if !padstack.attach_allowed {
            self.out.new_line();
            self.out.write("(attach off)");
        }
        self.out.end_scope();
    }

    /// `Shape.write_scope_int` for the flavours `CoordinateTransform.board_to_dsn_rel` produces:
    /// `IntBox` → `(rect …)`, `Circle` → `(circle …)`, every `PolylineShape` → `(polygon …)`
    /// over `corner_approx_arr()`.
    fn write_shape_int(&mut self, geometry: &PadGeometry, layer: i32) {
        let layer_name = self.input.board.layers().name(layer).unwrap_or("");
        match geometry {
            PadGeometry::Circle(circle) => {
                // The DSN number is a *diameter*: `board_to_dsn` writes `2 * radius` and
                // `transform_to_board` halves it again (`Circle.java:36`).
                let diameter = self.coordinate(2.0 * f64::from(circle.radius));
                let x = self.coordinate(f64::from(circle.center.x));
                let y = self.coordinate(f64::from(circle.center.y));
                self.out.new_line();
                self.out.write("(circle ");
                self.out
                    .write_identifier(layer_name, self.input.string_quote);
                self.out.write(" ");
                self.out.write_int(diameter);
                self.out.write(" ");
                self.out.write_int(x);
                self.out.write(" ");
                self.out.write_int(y);
                self.out.write(")");
            }
            PadGeometry::Box(shape) => self.write_rect_int(*shape, layer),
            PadGeometry::Octagon(octagon) => {
                // `PolylineShape.corner_approx_arr` walks `border_line_count()` corners, which
                // is 8 for an octagon — degenerate repeats included, as upstream writes them.
                let corners: Vec<FloatPoint> =
                    (0..8).map(|no| octagon.corner(no).to_float()).collect();
                self.write_polygon_int(&corners, layer);
            }
            PadGeometry::Simplex(simplex) => {
                let corners = simplex.corner_approx_arr();
                self.write_polygon_int(&corners, layer);
            }
        }
    }

    fn write_rect_int(&mut self, shape: IntBox, layer: i32) {
        let coordinates = [
            self.coordinate(f64::from(shape.ll.x)),
            self.coordinate(f64::from(shape.ll.y)),
            self.coordinate(f64::from(shape.ur.x)),
            self.coordinate(f64::from(shape.ur.y)),
        ];
        let layer_name = self.input.board.layers().name(layer).unwrap_or("");
        self.out.new_line();
        self.out.write("(rect ");
        self.out
            .write_identifier(layer_name, self.input.string_quote);
        for value in coordinates {
            self.out.write(" ");
            self.out.write_int(value);
        }
        self.out.write(")");
    }

    /// `Polygon.write_scope_int`: the aperture width is hard-coded to `0`.
    fn write_polygon_int(&mut self, corners: &[FloatPoint], layer: i32) {
        let layer_name = self.input.board.layers().name(layer).unwrap_or("");
        self.out.start_scope();
        self.out.write("polygon ");
        self.out
            .write_identifier(layer_name, self.input.string_quote);
        self.out.write(" 0");
        for corner in corners {
            let x = self.coordinate(corner.x);
            let y = self.coordinate(corner.y);
            self.out.new_line();
            self.out.write_int(x);
            self.out.write(" ");
            self.out.write_int(y);
        }
        self.out.end_scope();
    }

    // -----------------------------------------------------------------------------------------
    // network_out
    // -----------------------------------------------------------------------------------------

    fn write_network_out(&mut self) {
        self.out.start_scope();
        self.out.write("network_out ");
        for net_no in 1..=self.input.nets.max_net_no() {
            self.write_net(net_no as i32);
        }
        self.out.end_scope();
    }

    /// `SesWriter.writeNet` (§12.2 rule 6): the `(net <name>` header is written lazily, on the
    /// first item that is a wire, a via, or a conduction area on a *signal* layer.
    ///
    /// Items come in descending id order, which is upstream's `get_connectable_items` order
    /// (`Item.compareTo` is `other.id - this.id`, and `UndoableObjects` is a sorted map over
    /// it). The store's per-net index is ascending, hence the reverse.
    fn write_net(&mut self, net_no: i32) {
        let board = self.input.board;
        let mut header_written = false;
        for id in board.store().net_items(net_no).iter().rev().copied() {
            let Some(item) = board.store().get(id) else {
                continue;
            };
            // §10.3: `SYSTEM_FIXED` is board-owned geometry — keepouts, outline holes, locked
            // component copper — and never belongs in a session file.
            if item.fixed_state == FixedState::SystemFixed || !item.contains_net(net_no) {
                continue;
            }
            let kind = match &item.kind {
                ItemKind::Trace(_) => NetItem::Wire,
                ItemKind::Via(_) => NetItem::Via,
                ItemKind::ConductionArea(area) if board.layers().is_signal(area.layer) => {
                    NetItem::ConductionArea
                }
                _ => continue,
            };
            if !header_written {
                self.out.start_scope();
                self.out.write("net ");
                match self.input.nets.get(net_no) {
                    Some(net) => {
                        let quote = self.input.string_quote;
                        self.out.write_identifier(&net.name, quote);
                    }
                    None => self.warn(format!("SesWriter.writeNet: net {net_no} not found")),
                }
                header_written = true;
            }
            match kind {
                NetItem::Wire => self.write_wire(id),
                NetItem::Via => self.write_via(id),
                NetItem::ConductionArea => self.write_conduction_area(id),
            }
        }
        if header_written {
            self.out.end_scope();
        }
    }

    fn write_wire(&mut self, id: ItemId) {
        let board = self.input.board;
        // §12.3 runs before the corner walk because it needs `&mut self` for the contact
        // scratch buffer.
        let start_snap = self.snapped_endpoint(id, true);
        let end_snap = self.snapped_endpoint(id, false);

        let Some(item) = board.store().get(id) else {
            return;
        };
        let ItemKind::Trace(trace) = &item.kind else {
            return;
        };
        let layer_name = board.layers().name(trace.layer).unwrap_or("");
        let width = self.coordinate(f64::from(2 * trace.half_width));
        let corner_count = trace.corner_count();

        self.out.start_scope();
        self.out.write("wire");

        self.out.start_scope();
        self.out.write("path ");
        self.out
            .write_identifier(layer_name, self.input.string_quote);
        self.out.write(" ");
        self.out.write_int(width);
        let mut previous: Option<(f64, f64)> = None;
        for index in 0..corner_count {
            // The exact corner, not `corner_approx`: pull-tight's
            // diagonal∩diagonal corners sit on the half-integer grid, and
            // rounding them to the session's integers can move copper by 0.5
            // board units — enough to turn a clearance-exact gap into a
            // violation on re-import (Issue102-Mars-64-revE-rot00, +1v).
            let corner = if index == 0 {
                start_snap.unwrap_or_else(|| trace.polyline.corner(index).to_float())
            } else if index == corner_count - 1 {
                end_snap.unwrap_or_else(|| trace.polyline.corner(index).to_float())
            } else {
                trace.polyline.corner(index).to_float()
            };
            // §12.3: consecutive-duplicate elision, on the exact corners. Two
            // genuinely distinct corners — even 0.5 units apart — are kept;
            // only a true duplicate would leave a zero-length segment.
            let point = (
                self.scale.board_to_ses(corner.x),
                self.scale.board_to_ses(corner.y),
            );
            if previous == Some(point) {
                continue;
            }
            previous = Some(point);
            self.out.new_line();
            self.out.write(&self.coordinate_text(point.0));
            self.out.write(" ");
            self.out.write(&self.coordinate_text(point.1));
        }
        self.out.end_scope();

        self.write_fixed_state(item.fixed_state);
        self.out.end_scope();
    }

    fn write_via(&mut self, id: ItemId) {
        let input = self.input;
        let Some(item) = input.board.store().get(id) else {
            return;
        };
        let ItemKind::Via(via) = &item.kind else {
            return;
        };
        let x = self.coordinate(f64::from(via.center.x));
        let y = self.coordinate(f64::from(via.center.y));
        let name = input
            .library
            .via_padstack_name(input.padstacks, via.padstack_no);
        let Some(name) = name else {
            // Upstream writes `via.get_padstack().name` unconditionally and can therefore
            // produce a `(via …)` that references a padstack `library_out` never declared —
            // a file KiCad rejects outright. Dropping the via keeps the rest loadable.
            self.warn(format!(
                "SesWriter.writeVia: no library_out padstack matches the via at ({x}, {y}); \
                 via omitted"
            ));
            return;
        };
        let fixed_state = item.fixed_state;
        self.out.start_scope();
        self.out.write("via ");
        self.out.write_identifier(name, input.string_quote);
        self.out.write(" ");
        self.out.write_int(x);
        self.out.write(" ");
        self.out.write_int(y);
        self.write_fixed_state(fixed_state);
        self.out.end_scope();
    }

    /// `SesWriter.writeFixedState` — **not** `Wiring.writeFixedState`: anything at or below
    /// `SHOVE_FIXED` writes nothing, so `shove_fixed` does not survive a session file (§10.3).
    fn write_fixed_state(&mut self, fixed_state: FixedState) {
        if fixed_state <= FixedState::ShoveFixed {
            return;
        }
        self.out.new_line();
        self.out.write("(type ");
        if fixed_state == FixedState::SystemFixed {
            // Unreachable from `write_net`, which skips `SYSTEM_FIXED` items outright. Kept so
            // the mapping is complete.
            self.out.write("fix)");
        } else {
            self.out.write("protect)");
        }
    }

    /// `SesWriter.writeConductionArea`: a pour is a `(wire <shape>)` with no `(path …)`.
    ///
    /// > **Gap.** Upstream follows the boundary with one `(window <shape>)` per hole. This
    /// > port's [`fr_board::AreaShape`] has no with-holes variant — the importer already drops
    /// > pour windows with a warning (see the [`super`] module docs) — so none are written. A
    /// > pour therefore round-trips *larger* than the DSN's, never smaller, the same direction
    /// > the import gap goes.
    fn write_conduction_area(&mut self, id: ItemId) {
        let board = self.input.board;
        let Some(item) = board.store().get(id) else {
            return;
        };
        let ItemKind::ConductionArea(area) = &item.kind else {
            return;
        };
        if item.net_count() != 1 {
            let net_count = item.net_count();
            self.warn(format!(
                "SesWriter.writeConductionArea: unexpected net count {net_count}"
            ));
            return;
        }
        if area.area.rotation_deg != 0.0 || area.area.side_changed {
            // Only component-owned areas are ever placed, and a component never owns a pour.
            self.warn(
                "SesWriter.writeConductionArea: rotated or mirrored pour geometry is not \
                 serialised",
            );
            return;
        }
        let layer = area.layer;
        let translation = area.area.translation;
        match &area.area.relative_area {
            AreaShape::Box(shape) => {
                let shape = shape.translate_by(&Vector::Int(translation));
                self.out.start_scope();
                self.out.write("wire ");
                self.write_rect_int(shape, layer);
                self.out.end_scope();
            }
            AreaShape::Octagon(octagon) => {
                let corners: Vec<FloatPoint> =
                    (0..8).map(|no| octagon.corner(no).to_float()).collect();
                self.write_area_wire(&corners, translation, layer);
            }
            AreaShape::Simplex(simplex) => {
                let corners = simplex.corner_approx_arr();
                self.write_area_wire(&corners, translation, layer);
            }
            AreaShape::Polygon(polygon) => {
                let corners: Vec<FloatPoint> = polygon
                    .corner_array()
                    .iter()
                    .map(|c| c.to_float())
                    .collect();
                self.write_area_wire(&corners, translation, layer);
            }
            AreaShape::Convex(pieces) => {
                // A pre-split area has no single border to write. Upstream kept the
                // `PolylineArea` and wrote `get_border()`; the decomposition threw that away,
                // so the pieces go out one `(wire …)` each.
                for piece in pieces {
                    let corners = piece.corner_approx_arr();
                    self.write_area_wire(&corners, translation, layer);
                }
            }
        }
    }

    fn write_area_wire(&mut self, corners: &[FloatPoint], translation: IntVector, layer: i32) {
        let translated: Vec<FloatPoint> = corners
            .iter()
            .map(|corner| {
                FloatPoint::new(
                    corner.x + f64::from(translation.x),
                    corner.y + f64::from(translation.y),
                )
            })
            .collect();
        self.out.start_scope();
        self.out.write("wire ");
        self.write_polygon_int(&translated, layer);
        self.out.end_scope();
    }

    // -----------------------------------------------------------------------------------------
    // §12.3 — endpoint snapping
    // -----------------------------------------------------------------------------------------

    /// `SesWriter.snappedEndpoint` (v2-only, `SesWriter.java:426-453`).
    ///
    /// Freerouting approximates a pad with an octagon. A trace endpoint can land inside that
    /// octagon but outside the importing tool's real pad polygon, and KiCad then reports the net
    /// as unconnected. Snapping the endpoint to the contacted drill item's centre — the same
    /// coordinate [`SesFileWriter::write_via`] emits — closes those sub-pad gaps.
    ///
    /// The guard is what makes it safe: snap only when the endpoint is already within the pad's
    /// **inradius** of the centre (`border_distance`), so the last segment cannot leave the pad
    /// or fold across a neighbour. An endpoint already at the centre (within half a board unit)
    /// returns `None` — there is nothing to fix.
    ///
    /// # It cannot fire, and that is upstream's shape too
    ///
    /// The candidate set is `Trace.get_start_contacts()`, i.e.
    /// `get_normal_contacts(first_corner(), false)` (`Trace.java:101-103`), and that method's
    /// drill-item branch is `p_point.equals(curr_drill_item.get_center())` (`Trace.java:189`) —
    /// an **exact** match, mirrored here by
    /// [`connectivity::touches_at`](fr_board::connectivity). So a pin or via is a contact only
    /// when the endpoint *already is* its centre, and then `center_distance == 0 <= 0.5` returns
    /// `None` on the next line. An endpoint inside the pad but off-centre — precisely the case
    /// §12.3 describes — has no drill contact at all, so the loop never sees it.
    ///
    /// The transcription is kept faithful rather than deleted: it costs one already-warm search
    /// tree query per trace end, it is what the release ships, and the day the contact predicate
    /// grows a tolerance (or the router starts ending traces off-centre inside a pad) this
    /// becomes live without anyone having to rediscover §12.3.
    /// `ses_format_test.rs::an_endpoint_inside_the_pad_is_not_snapped_because_it_is_not_a_contact`
    /// pins the current behaviour.
    fn snapped_endpoint(&mut self, id: ItemId, start_side: bool) -> Option<FloatPoint> {
        let board = self.input.board;
        let item = board.store().get(id)?;
        let ItemKind::Trace(trace) = &item.kind else {
            return None;
        };
        let layer = trace.layer;
        let corner = if start_side {
            trace.first_corner()
        } else {
            trace.last_corner()
        };
        let corner_float = corner.to_float();

        let mut contacts = std::mem::take(&mut self.contacts);
        board.normal_contacts_at(id, &corner, &mut contacts);
        let mut result = None;
        for contact in contacts.iter().copied() {
            let Some(item) = board.store().get(contact) else {
                continue;
            };
            // Upstream's `instanceof DrillItem`: pins and vias, the two items with a drill
            // centre and a per-layer pad.
            let (center, padstack_no, layers) = match &item.kind {
                ItemKind::Pin(pin) => (pin.center, pin.padstack_no, pin.layers),
                ItemKind::Via(via) => (via.center, via.padstack_no, via.layers),
                _ => continue,
            };
            if !layers.contains(layer) {
                continue;
            }
            let Some(pad_shape) = board
                .padstacks()
                .get(padstack_no)
                .and_then(|padstack| padstack.shape_on_layer(layer))
            else {
                continue;
            };
            let center_float = center.to_float();
            let center_distance = corner_float.distance(&center_float);
            if center_distance <= 0.5 {
                // Already at the centre within rounding — nothing to fix. Upstream returns
                // outright rather than trying the next contact.
                break;
            }
            // Pad shapes are stored relative to the drill centre, so the inradius measured at
            // the centre of the placed pad is the inradius at the origin of the stored one.
            if center_distance <= border_distance_at_origin(pad_shape) {
                result = Some(center_float);
                break;
            }
        }
        contacts.clear();
        self.contacts = contacts;
        result
    }
}

/// Which of the three `network_out` item flavours an item is.
enum NetItem {
    Wire,
    Via,
    ConductionArea,
}

/// `Shape.border_distance(center)` on a pad stored relative to its drill centre.
fn border_distance_at_origin(shape: &TileShape) -> f64 {
    let origin = FloatPoint::new(0.0, 0.0);
    match shape {
        TileShape::Box(b) => b.border_distance(&origin),
        TileShape::Oct(o) => o.border_distance(&origin),
        TileShape::Simplex(s) => s.border_distance(&origin),
    }
}

/// `SesWriter.formatPlacementRotation` (v2-only, `SesWriter.java:259-269`).
///
/// Whole degrees print as integers (`0`, `339`), fractional degrees with the minimum precision
/// that keeps them (`338.5`, never `338.500`). v1.9 wrote `(int) Math.round(rotation)` and lost
/// every fractional rotation.
///
/// Upstream pins `Locale.ENGLISH` so the decimal separator is `.` whatever the JVM locale is.
/// Rust's `{:.3}` is locale-independent, so the pin is a no-op here — but the *rounding* is not:
/// `Math.rint` breaks ties to even ([`java_rint`]), where `f64::round` breaks them away from
/// zero.
pub fn format_placement_rotation(degrees: f64) -> String {
    let rounded = java_rint(degrees * 1000.0) / 1000.0;
    if (rounded - java_rint(rounded)).abs() < 1e-9 {
        return format!("{rounded:.0}");
    }
    let formatted = format!("{rounded:.3}");
    if formatted.contains('.') {
        formatted
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    } else {
        formatted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quoted(name: &str) -> String {
        let mut out = IndentWriter::new();
        out.write_identifier(name, "\"");
        out.out
    }

    #[test]
    fn reserved_characters_are_the_ses_set_not_the_dsn_set() {
        // §12: the SES set adds `/ ~ { }` to the DSN writer's `( ) SPACE ; - _`.
        assert!(needs_quotes("/CLK"));
        assert!(needs_quotes("~{RESET}"));
        assert!(needs_quotes("Sheet/Sub/NET"));
        assert!(needs_quotes("a b"));
        assert!(needs_quotes("a;b"));
        assert!(needs_quotes("a-b"));
        assert!(needs_quotes("a_b"));
        assert!(needs_quotes("a(b"));
        assert!(needs_quotes("a)b"));
        assert!(needs_quotes("a{b"));
        assert!(needs_quotes("a}b"));
        // Nothing else does.
        assert!(!needs_quotes("F.Cu"));
        assert!(!needs_quotes("GND"));
        assert!(!needs_quotes("J2"));
        assert!(!needs_quotes("a+b"));
        assert!(!needs_quotes("Round[A]Pad"));
    }

    #[test]
    fn non_ascii_and_leading_digits_force_quotes() {
        // `for (byte ch : name.getBytes(UTF_8)) if (ch <= 0)`.
        assert!(needs_quotes("Паяльная"));
        // `name.matches("^-?\\d.*")`.
        assert!(needs_quotes("1"));
        assert!(needs_quotes("12V"));
        assert!(!needs_quotes("V12"));
    }

    #[test]
    fn empty_names_are_quoted_so_kicad_can_parse_them() {
        // Regression: cla45-compute-hdi6cs.dsn declares `(image ""`; writing the package name as
        // zero bytes produced `(component `, which KiCad's specctra parser rejects.
        assert!(needs_quotes(""));
        assert_eq!(quoted(""), "\"\"");
    }

    #[test]
    fn identifiers_are_quoted_with_the_dsn_string_quote() {
        assert_eq!(quoted("GND"), "GND");
        assert_eq!(quoted("/CLK"), "\"/CLK\"");
        assert_eq!(quoted("Via[0-1]_800:400_um"), "\"Via[0-1]_800:400_um\"");
        // An embedded quote character is deleted, not escaped.
        assert_eq!(quoted("a\"b"), "ab");

        let mut out = IndentWriter::new();
        out.write_identifier("/CLK", "'");
        assert_eq!(out.out, "'/CLK'");
    }

    #[test]
    fn placement_rotation_keeps_fractions_and_drops_trailing_zeros() {
        // §12.4's worked examples.
        assert_eq!(format_placement_rotation(0.0), "0");
        assert_eq!(format_placement_rotation(339.0), "339");
        assert_eq!(format_placement_rotation(338.5), "338.5");
        assert_eq!(format_placement_rotation(338.500_1), "338.5");
        assert_eq!(format_placement_rotation(90.0), "90");
        assert_eq!(format_placement_rotation(12.345), "12.345");
        assert_eq!(format_placement_rotation(12.345_6), "12.346");
        // v1.9's `(int) Math.round(rotation)` would have written "339" for this one.
        assert_eq!(format_placement_rotation(338.75), "338.75");
    }

    #[test]
    fn indent_writer_matches_indent_file_writer() {
        let mut out = IndentWriter::new();
        out.start_scope_opt(false);
        out.write("session ");
        out.write("\"x.ses\"");
        out.new_line();
        out.write("(base_design \"x.dsn\")");
        out.start_scope();
        out.write("was_is");
        out.end_scope();
        out.end_scope();
        assert_eq!(
            out.out,
            "(session \"x.ses\"\n  (base_design \"x.dsn\")\n  (was_is\n  )\n)"
        );
    }
}
