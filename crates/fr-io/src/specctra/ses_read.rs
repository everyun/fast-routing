//! `io/specctra/SesReader.java` — re-importing a Specctra session (`.ses`)
//! (`dsn-board-semantics.md` §12.5).
//!
//! The reader walks `(session … (routes … (network_out (net …))))` and inserts the wires and
//! vias it finds into an existing board. `placement` and `was_is` are skipped outright, exactly
//! as upstream does (`SesReader.java:148-155`) — a session file cannot move components here
//! because the board's pads are already placed.
//!
//! # Two modes, and why
//!
//! Upstream's re-import is *deliberately lossy*: it exists so a user can pull a routing result
//! back into the GUI and not have the router touch it again. That makes three choices which are
//! wrong for a round-trip check but right for KiCad-compatible behaviour, so they are switched
//! on [`SesCompat`]:
//!
//! | | [`SesCompat::Upstream`] | [`SesCompat::Lossless`] (G1.2c) |
//! |---|---|---|
//! | fixed state | everything `USER_FIXED` (`SesReader.java:325`, `:404`) | from `(type …)`, i.e. the inverse of `SesWriter.writeFixedState` |
//! | clearance classes | the **default** net class's `[TRACE]`/`[VIA]`, whatever the net's own class is (`SesReader.java:316-321`, `:395-400`) | the item's own net class |
//! | duplicate vias | none — `Wiring.readViaScope`'s `viaExists` check is v2-only and *not* in `SesReader` | skipped, matching the DSN wiring path (§10.2) |
//! | trace insertion | `insert_trace`, i.e. normalize each trace as it lands (`SesReader.java:323`) | `insert_trace_without_cleaning` then one `normalize_traces` per net, matching §10.4 |
//!
//! **G1.2c uses [`SesCompat::Lossless`].** The KiCad-compat path — writing a session for KiCad
//! and reading back what the user edited — is [`SesCompat::Upstream`].
//!
//! # One thing neither mode reproduces
//!
//! The session scale. See [`SesScale`](super::ses_common::SesScale): upstream's reader divides
//! by the same factor its writer divides by, so it is only its own inverse when the factor is 1.
//! Both modes here multiply.

use fr_board::item::Via;
use fr_board::{Board, FixedState, ItemKind, LayerRange, NetNos};
use fr_geometry::planar::{IntPoint, Polyline};
use fr_rules::{BoardRules, ItemClass};

use super::build::ImportedDesign;
use super::jround::jround;
use super::lex::{Kw, LexState, Lexer, Token};
use super::ses_common::{SesLibrary, SesScale};

/// How faithfully to reproduce upstream's re-import. See the module docs for the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SesCompat {
    /// Read back exactly what [`super::ses_write`] wrote: `(type …)`-derived fixed states, the
    /// item's own net-class clearances, duplicate vias skipped, normalization deferred.
    #[default]
    Lossless,
    /// `SesReader.java` verbatim: everything `USER_FIXED`, default-class clearances, no via
    /// dedup, per-trace normalization.
    Upstream,
}

/// Reader knobs.
#[derive(Debug, Clone, Copy, Default)]
pub struct SesReadOptions {
    /// Which divergence set to apply.
    pub compat: SesCompat,
}

/// `io/specctra/SesImportSummary`, plus the warnings upstream only logs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SesImportSummary {
    /// Wires inserted into the board.
    pub wires_imported: usize,
    /// Vias inserted into the board.
    pub vias_imported: usize,
    /// Records skipped because of a parse or geometry error. Non-zero means items were dropped;
    /// the board is otherwise intact.
    pub errors_encountered: usize,
    /// Everything the reader wanted to complain about.
    pub warnings: Vec<String>,
}

/// Why a session read failed outright.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SesReadError {
    /// The stream did not start with `(session <name>`.
    NotASesFile,
}

impl std::fmt::Display for SesReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SesReadError::NotASesFile => write!(
                f,
                "not a Specctra session file: expected a '(session <name>' header"
            ),
        }
    }
}

impl std::error::Error for SesReadError {}

/// The board and the design metadata a session file has to be resolved against.
///
/// `SesReader` reaches into `board.rules.nets`, `board.library.padstacks` and
/// `board.communication` for the same four things; here they are explicit because
/// [`fr_board::Board`] deliberately carries neither names nor units.
pub struct SesTarget<'a> {
    /// The board wires and vias are added to.
    pub board: &'a mut Board,
    /// Net names → numbers, and the net classes the clearance classes come from.
    pub rules: &'a BoardRules,
    /// The DSN padstack library, for `(via <padstackName> …)`.
    pub padstacks: &'a [super::build::ImportedPadstack],
    /// The name binding between `padstacks` and the board's padstack table.
    pub library: &'a SesLibrary,
    /// `(resolution … <n>)` of the *design*, not of the session file.
    pub resolution: i32,
    /// The §3.2 board scale factor.
    pub board_scale_factor: f64,
    /// `(control (via_at_smd on))` — with the padstack's own `attach`, this decides whether a
    /// re-imported via may land on a same-net SMD pad.
    pub via_at_smd_allowed: bool,
}

impl<'a> SesTarget<'a> {
    /// Reads a session back into the board it came from.
    ///
    /// The destructuring is the point: `design.board` and `design.rules` are disjoint fields, so
    /// one `&mut ImportedDesign` yields both the mutable board and the shared rules.
    /// `library` is passed separately because [`SesLibrary::from_design`] needs the design
    /// immutably and must therefore be built first.
    pub fn for_design(design: &'a mut ImportedDesign, library: &'a SesLibrary) -> SesTarget<'a> {
        let ImportedDesign {
            board,
            rules,
            padstacks,
            resolution,
            scale_factor,
            via_at_smd_allowed,
            ..
        } = design;
        SesTarget {
            board,
            rules,
            padstacks,
            library,
            resolution: *resolution,
            board_scale_factor: *scale_factor,
            via_at_smd_allowed: *via_at_smd_allowed,
        }
    }
}

/// `SesReader.read(in, board)` with [`SesCompat::Lossless`].
pub fn read_session(src: &[u8], target: SesTarget<'_>) -> Result<SesImportSummary, SesReadError> {
    read_session_with_options(src, target, SesReadOptions::default())
}

/// `SesReader.read(in, board)`.
pub fn read_session_with_options(
    src: &[u8],
    target: SesTarget<'_>,
    options: SesReadOptions,
) -> Result<SesImportSummary, SesReadError> {
    let scale = SesScale::new(target.board_scale_factor, target.resolution);
    let mut reader = SesFileReader {
        lexer: Lexer::new(src),
        target,
        options,
        scale,
        summary: SesImportSummary::default(),
        touched_nets: Vec::new(),
    };
    reader.read_session_scope()?;
    reader.finish();
    Ok(reader.summary)
}

// =============================================================================================

struct SesFileReader<'a, 'b> {
    lexer: Lexer<'a>,
    target: SesTarget<'b>,
    options: SesReadOptions,
    scale: SesScale,
    summary: SesImportSummary,
    /// Nets that received a wire, for the deferred normalization pass.
    touched_nets: Vec<i32>,
}

impl SesFileReader<'_, '_> {
    fn warn(&mut self, message: impl Into<String>) {
        self.summary.warnings.push(message.into());
    }

    /// `ScopeKeyword.skipScope`: bracket counting with `yybegin(NAME)` before every token, so
    /// nothing inside a skipped scope is interpreted.
    fn skip_scope(&mut self) {
        let mut depth = 1i32;
        while depth > 0 {
            self.lexer.begin(LexState::Name);
            match self.lexer.next_token() {
                None => return,
                Some(Token::Open) => depth += 1,
                Some(Token::Close) => depth -= 1,
                _ => {}
            }
        }
    }

    /// `SesReader.processSessionScope` — validate `(session <name>`, then dispatch subscopes.
    fn read_session_scope(&mut self) -> Result<(), SesReadError> {
        if self.lexer.next_token() != Some(Token::Open) {
            return Err(SesReadError::NotASesFile);
        }
        if self.lexer.next_token() != Some(Token::Kw(Kw::Session)) {
            return Err(SesReadError::NotASesFile);
        }
        // Consume the session name. `session` is not one of the DFA's `yybegin(NAME)` keywords,
        // so upstream switches the scanner by hand (`SesReader.java:106`).
        self.lexer.begin(LexState::Name);
        if self.lexer.next_token().is_none() {
            return Err(SesReadError::NotASesFile);
        }

        let mut next: Option<Token> = None;
        loop {
            let previous = next;
            next = self.lexer.next_token();
            match &next {
                None => return Ok(()),
                Some(Token::Close) => return Ok(()),
                _ => {}
            }
            if previous == Some(Token::Open) {
                if next == Some(Token::Kw(Kw::Routes)) {
                    self.read_routes_scope();
                } else {
                    // `placement`, `was_is`, and anything else.
                    self.skip_scope();
                }
            }
        }
    }

    fn read_routes_scope(&mut self) {
        let mut next: Option<Token> = None;
        loop {
            let previous = next;
            next = self.lexer.next_token();
            match &next {
                None => {
                    self.warn("SesReader.read_routes_scope: unexpected end of file");
                    return;
                }
                Some(Token::Close) => return,
                _ => {}
            }
            if previous == Some(Token::Open) {
                if next == Some(Token::Kw(Kw::NetworkOut)) {
                    self.read_network_scope();
                } else {
                    self.skip_scope();
                }
            }
        }
    }

    fn read_network_scope(&mut self) {
        let mut next: Option<Token> = None;
        loop {
            let previous = next;
            next = self.lexer.next_token();
            match &next {
                None => {
                    self.warn("SesReader.read_network_scope: unexpected end of file");
                    return;
                }
                Some(Token::Close) => return,
                _ => {}
            }
            if previous == Some(Token::Open) {
                if next == Some(Token::Kw(Kw::Net)) {
                    self.read_net_scope();
                } else {
                    self.skip_scope();
                }
            }
        }
    }

    /// `SesReader.processNetScope`.
    ///
    /// The net is looked up as `nets.get(name, 1)` — **subnet 1 only**, unlike the DSN wiring
    /// scope's `getSubnets`, which returns every subnet of the name when none is given (§10.1).
    /// A session file has no subnet syntax, so there is nothing to widen it to.
    fn read_net_scope(&mut self) {
        // `net` is a `yybegin(NAME)` keyword, so a numeric net name arrives as a string.
        let Some(net_name) = self.lexer.next_token().and_then(|token| match token {
            Token::Str(name) => Some(name),
            _ => None,
        }) else {
            self.warn("SesReader.read_net_scope: net name expected");
            self.summary.errors_encountered += 1;
            return;
        };
        self.lexer.set_scope_identifier(&net_name);

        let Some(net) = self.target.rules.nets.get_by_name_and_subnet(&net_name, 1) else {
            self.warn(format!("SesReader: net not found: '{net_name}' — skipping"));
            self.summary.errors_encountered += 1;
            self.skip_scope();
            return;
        };
        let net_no = net.net_number;
        let net_class_index = net.net_class_index;

        let mut next: Option<Token> = Some(Token::Str(net_name));
        loop {
            let previous = next;
            next = self.lexer.next_token();
            match &next {
                None => return,
                Some(Token::Close) => return,
                _ => {}
            }
            if previous == Some(Token::Open) {
                match next {
                    Some(Token::Kw(Kw::Wire)) => {
                        if !self.read_wire_scope(net_no, net_class_index) {
                            self.summary.errors_encountered += 1;
                        }
                    }
                    Some(Token::Kw(Kw::Via)) => {
                        if !self.read_via_scope(net_no, net_class_index) {
                            self.summary.errors_encountered += 1;
                        }
                    }
                    _ => self.skip_scope(),
                }
            }
        }
    }

    /// `SesReader.processWireScope`.
    ///
    /// Returning `true` without importing anything is upstream's shrug, and it covers three
    /// cases that are worth telling apart in a log even though none is an error:
    ///
    /// * a `(wire <shape>)` with no `(path …)` at all — a conduction area, which comes back from
    ///   the DSN rather than from the session;
    /// * a `(path …)` `Shape.read_polygon_path_scope` refuses, i.e. one with fewer than two
    ///   corners or on an unknown layer (`Shape.java`, "Single-point paths are not valid traces,
    ///   skip them"). §12.3's duplicate-corner elision can *produce* a single-corner path when
    ///   every corner of a trace rounds onto the same session coordinate, so a writer/reader
    ///   pair will hit this on genuinely sub-unit copper;
    /// * a path with two or more corners that still collapses in
    ///   `insert_trace_without_cleaning` — the corners rounded together on the way *back* to
    ///   board units, or the trace is a zero-length loop below `USER_FIXED`
    ///   (`BasicBoard.java:244-255`). `SesReader.processWireScope` calls the `void`
    ///   `insert_trace` and has no way to see this: it increments `wiresImported` and returns
    ///   `true` regardless (`SesReader.java:293-297`), and only a thrown exception makes it
    ///   report an error. The counters are `SesImportSummary`'s and are compared against the
    ///   jar's, so they are kept upstream-exact here; the *warning* is ours, and is the thing to
    ///   look at when a session's copper does not all survive a round trip.
    ///
    /// A caller that needs to know whether the board really received everything the file
    /// describes must compare the board, not the counters — `bench/tools/g21_judge.py` does that
    /// with the router's own in-process referee line.
    fn read_wire_scope(&mut self, net_no: i32, net_class_index: usize) -> bool {
        let mut path: Option<(i32, Vec<f64>)> = None;
        let mut fixed = FixedState::Unfixed;
        let mut next: Option<Token> = None;
        loop {
            let previous = next;
            next = self.lexer.next_token();
            match &next {
                None => {
                    self.warn("SesReader.read_wire_scope: unexpected end of file");
                    return false;
                }
                Some(Token::Close) => break,
                _ => {}
            }
            if previous == Some(Token::Open) {
                match next {
                    Some(Token::Kw(Kw::PolygonPath)) => path = self.read_path_scope(),
                    Some(Token::Kw(Kw::Type)) => fixed = self.read_type_scope(),
                    _ => self.skip_scope(),
                }
            }
        }

        let Some((layer, values)) = path else {
            return true;
        };
        // `values[0]` is the path width; the rest are the corner coordinates.
        let width = values[0];
        let coordinates = &values[1..];
        if coordinates.len() < 4 || coordinates.len() % 2 != 0 {
            self.warn("SesReader.read_wire_scope: a path needs at least two corners");
            return false;
        }

        let corners: Vec<(f64, f64)> = coordinates
            .chunks_exact(2)
            .map(|pair| {
                (
                    self.scale.ses_to_board(pair[0]),
                    self.scale.ses_to_board(pair[1]),
                )
            })
            .collect();
        // Corners carrying an exact half unit (the writer's
        // `SesFileWriter::coordinate_text`) rebuild the polyline they came
        // from — a diagonal∩diagonal corner rounded to the integer lattice
        // here would move copper by up to 0.5 board units, the Issue102
        // round-trip violation. Anything else is rounded exactly as
        // upstream's `SesReader` rounds it.
        let all_integral = corners
            .iter()
            .all(|&(x, y)| (x.round() - x).abs() < 1e-6 && (y.round() - y).abs() < 1e-6);
        let polyline = if all_integral {
            Polyline::from_int_points(
                &corners
                    .iter()
                    .map(|&(x, y)| IntPoint::new(x.round() as i32, y.round() as i32))
                    .collect::<Vec<_>>(),
            )
        } else {
            Polyline::from_exact_corners(&corners).unwrap_or_else(|| {
                Polyline::from_int_points(
                    &corners
                        .iter()
                        .map(|&(x, y)| IntPoint::new(jround(x) as i32, jround(y) as i32))
                        .collect::<Vec<_>>(),
                )
            })
        };
        let half_width = jround(self.scale.ses_to_board(width) / 2.0) as i32;
        let clearance_class = self.item_clearance_class(net_class_index, ItemClass::Trace);
        let fixed = match self.options.compat {
            SesCompat::Upstream => FixedState::UserFixed,
            SesCompat::Lossless => fixed,
        };

        let inserted = match self.options.compat {
            // `SesReader.java:323` uses the *cleaning* insert.
            SesCompat::Upstream => self.target.board.insert_trace(
                polyline,
                layer,
                half_width,
                NetNos::from_slice(&[net_no]),
                clearance_class,
                fixed,
                None,
            ),
            // §10.4: the wiring path inserts raw and defers normalization, "because cycles may
            // be removed premature".
            SesCompat::Lossless => self.target.board.insert_trace_without_cleaning(
                polyline,
                layer,
                half_width,
                NetNos::from_slice(&[net_no]),
                clearance_class,
                fixed,
            ),
        };
        if inserted.is_none() {
            // Fewer than two corners after rounding, or a zero-length unfixed trace. Upstream
            // cannot see this and counts the wire anyway; see the doc comment.
            self.warn("SesReader.read_wire_scope: degenerate wire skipped");
            self.summary.wires_imported += 1;
            return true;
        }
        if !self.touched_nets.contains(&net_no) {
            self.touched_nets.push(net_no);
        }
        self.summary.wires_imported += 1;
        true
    }

    /// `Shape.read_polygon_path_scope`, reduced to what a session file can hold: a layer name, a
    /// width, and the coordinates.
    fn read_path_scope(&mut self) -> Option<(i32, Vec<f64>)> {
        // `path` enters `LAYER_NAME`; upstream reads the layer through the raw-buffer bypass so
        // a numeric layer name survives (§2.3).
        let layer_name = self.lexer.next_string_default();
        let layer = self
            .target
            .board
            .layers()
            .iter()
            .find(|(_, layer)| layer.name == layer_name)
            .map(|(index, _)| index);
        let mut values: Vec<f64> = Vec::new();
        let mut malformed = false;
        loop {
            let mut token = self.lexer.next_token();
            if token == Some(Token::Open) {
                self.skip_scope();
                token = self.lexer.next_token();
            }
            match token {
                Some(Token::Close) | None => break,
                Some(ref value) => match value.as_number() {
                    Some(number) => values.push(number),
                    None => malformed = true,
                },
            }
        }
        if malformed {
            self.warn("SesReader.read_path_scope: number expected");
            return None;
        }
        // Upstream's order: the corner count is checked before the layer, so a one-corner path
        // on an unknown layer logs the corner-count message.
        if values.len() < 5 {
            // "Single-point paths are not valid traces, skip them" — upstream logs this at
            // debug level and lets `processWireScope` treat it as a conduction area.
            self.warn(format!(
                "SesReader.read_path_scope: skipping a path with {} corner(s) on layer \
                 '{layer_name}'",
                values.len().saturating_sub(1) / 2
            ));
            return None;
        }
        let Some(layer) = layer else {
            self.warn(format!(
                "SesReader.read_path_scope: unknown layer '{layer_name}'"
            ));
            return None;
        };
        Some((layer, values))
    }

    /// `Wiring.calcFixed`, restricted to the two tokens the SES writer emits.
    ///
    /// `fix` → `SYSTEM_FIXED`, `normal` → `UNFIXED`, `shove_fixed` → `SHOVE_FIXED`, everything
    /// else — `protect` included — → `USER_FIXED` (§10.3).
    fn read_type_scope(&mut self) -> FixedState {
        let fixed = match self.lexer.next_token() {
            Some(Token::Kw(Kw::Fix)) => FixedState::SystemFixed,
            Some(Token::Kw(Kw::Normal)) => FixedState::Unfixed,
            Some(Token::Kw(Kw::ShoveFixed)) => FixedState::ShoveFixed,
            // An empty `(type)` — the scope is already closed, so consuming another token would
            // eat the wire's closing bracket.
            Some(Token::Close) => return FixedState::UserFixed,
            None => return FixedState::UserFixed,
            _ => FixedState::UserFixed,
        };
        if self.lexer.next_token() != Some(Token::Close) {
            self.skip_scope();
        }
        fixed
    }

    /// `SesReader.processViaScope`.
    fn read_via_scope(&mut self, net_no: i32, net_class_index: usize) -> bool {
        // `via` is a `yybegin(NAME)` keyword.
        let Some(padstack_name) = self.lexer.next_token().and_then(|token| match token {
            Token::Str(name) => Some(name),
            _ => None,
        }) else {
            self.warn("SesReader.read_via_scope: padstack name expected");
            return false;
        };
        self.lexer.set_scope_identifier(&padstack_name);

        let mut location = [0.0f64; 2];
        for slot in location.iter_mut() {
            match self.lexer.next_token().and_then(|t| t.as_number()) {
                Some(number) => *slot = number,
                None => {
                    self.warn(format!(
                        "SesReader.read_via_scope: number expected at '{padstack_name}'"
                    ));
                    return false;
                }
            }
        }

        // Trailing sub-scopes: `(type …)`, `(lock_type …)`, anything a future writer adds.
        let mut fixed = FixedState::Unfixed;
        let mut token = self.lexer.next_token();
        while token == Some(Token::Open) {
            match self.lexer.next_token() {
                Some(Token::Kw(Kw::Type)) => fixed = self.read_type_scope(),
                _ => self.skip_scope(),
            }
            token = self.lexer.next_token();
        }
        if token != Some(Token::Close) {
            self.warn(format!(
                "SesReader.read_via_scope: closing bracket expected at '{padstack_name}'"
            ));
            return false;
        }

        let Some(resolved) = self.target.library.resolve_or_insert(
            self.target.padstacks,
            &padstack_name,
            &mut *self.target.board,
        ) else {
            self.warn(format!(
                "SesReader.read_via_scope: via padstack not found: {padstack_name}"
            ));
            return false;
        };

        let center = IntPoint::new(
            jround(self.scale.ses_to_board(location[0])) as i32,
            jround(self.scale.ses_to_board(location[1])) as i32,
        );
        let clearance_class = self.item_clearance_class(net_class_index, ItemClass::Via);
        let fixed = match self.options.compat {
            SesCompat::Upstream => FixedState::UserFixed,
            SesCompat::Lossless => fixed,
        };
        let layers = LayerRange::new(resolved.from_layer, resolved.to_layer);
        let nets = NetNos::from_slice(&[net_no]);

        if self.options.compat == SesCompat::Lossless && self.via_exists(&center, &layers, &nets) {
            // §10.2's `viaExists`, which `SesReader` does not have: the DSN wiring reader skips
            // a duplicate via with a warning, and a session file re-read into a board that
            // already carries the same via would otherwise stack them.
            self.warn(format!(
                "SesReader.read_via_scope: duplicate via skipped at ({}, {})",
                center.x, center.y
            ));
            return true;
        }

        self.target.board.insert_via(
            Via {
                center,
                padstack_no: resolved.board_padstack_no,
                layers,
                // `SesReader.java:406` hard-codes `attachAllowed = true`; the DSN wiring path
                // (§10.2) conjoins `(control (via_at_smd …))` with the padstack's own flag, and
                // that is what the obstacle matrix expects to see.
                attach_allowed: match self.options.compat {
                    SesCompat::Upstream => true,
                    SesCompat::Lossless => {
                        self.target.via_at_smd_allowed && resolved.attach_allowed
                    }
                },
                is_escape_via: false,
                escape_via_smd_layer: -1,
            },
            nets,
            clearance_class,
            fixed,
        );
        self.summary.vias_imported += 1;
        true
    }

    /// `Wiring.viaExists` (`Wiring.java:238-255`): same centre, same layer span, same net set.
    fn via_exists(&self, center: &IntPoint, layers: &LayerRange, nets: &NetNos) -> bool {
        let board: &Board = &*self.target.board;
        nets.iter().copied().any(|net_no| {
            board
                .store()
                .net_items(net_no)
                .iter()
                .filter_map(|id| board.store().get(*id))
                .any(|item| match &item.kind {
                    ItemKind::Via(via) => {
                        via.center == *center
                            && via.layers.first == layers.first
                            && via.layers.last == layers.last
                            && item.nets.as_slice() == nets.as_slice()
                    }
                    _ => false,
                })
        })
    }

    /// The clearance class a re-imported item gets.
    ///
    /// Upstream reads the **default** net class's matrix whatever net the item is on
    /// (`SesReader.java:316-321`, `:395-400`) — a divergence candidate, because it silently
    /// widens or narrows every re-imported item to the default clearance.
    fn item_clearance_class(&self, net_class_index: usize, item_class: ItemClass) -> i32 {
        let net_class = match self.options.compat {
            SesCompat::Upstream => self.target.rules.get_default_net_class(),
            SesCompat::Lossless => self
                .target
                .rules
                .net_classes
                .get(net_class_index)
                .unwrap_or_else(|| self.target.rules.get_default_net_class()),
        };
        net_class.default_item_clearance_classes.get(item_class)
    }

    /// `Wiring.readScope`'s tail: one `normalizeAllTraces` after the whole scope, not one per
    /// trace (§10.4). Only [`SesCompat::Lossless`] defers; [`SesCompat::Upstream`] already
    /// normalized inside `insert_trace`.
    fn finish(&mut self) {
        if self.options.compat != SesCompat::Lossless {
            return;
        }
        let nets = std::mem::take(&mut self.touched_nets);
        for net_no in &nets {
            self.target.board.normalize_traces(*net_no);
        }
        self.touched_nets = nets;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_design() -> ImportedDesign {
        // A two-layer board with no items — enough to exercise the header check.
        super::super::import(
            b"(pcb test\n\
              (parser (string_quote \") (space_in_quoted_tokens on))\n\
              (resolution um 10)\n\
              (structure\n\
                (layer F.Cu (type signal))\n\
                (layer B.Cu (type signal))\n\
                (boundary (rect pcb 0 0 10000 10000))\n\
              )\n\
              (library)\n\
              (network)\n\
              (wiring)\n\
            )",
        )
        .expect("synthetic board should import")
    }

    #[test]
    fn a_non_session_stream_is_rejected() {
        let mut design = empty_design();
        let library = SesLibrary::from_design(&design);
        let target = SesTarget::for_design(&mut design, &library);
        assert_eq!(
            read_session(b"garbage", target),
            Err(SesReadError::NotASesFile)
        );
    }

    #[test]
    fn placement_and_was_is_are_skipped_not_parsed() {
        // `SesReader.java:148-155`: only `routes` is dispatched. A `placement` scope naming a
        // component that does not exist must not produce an error.
        let mut design = empty_design();
        let library = SesLibrary::from_design(&design);
        let target = SesTarget::for_design(&mut design, &library);
        let summary = read_session(
            b"(session \"test.ses\"\n\
              (base_design \"test.dsn\")\n\
              (placement (resolution um 10) (component X (place NOPE 0 0 front 0)))\n\
              (was_is)\n\
              (routes (resolution um 10) (parser) (library_out ) (network_out ))\n\
            )",
            target,
        )
        .expect("a well-formed session should parse");
        assert_eq!(summary.errors_encountered, 0);
        assert_eq!(summary.wires_imported, 0);
        assert_eq!(summary.vias_imported, 0);
    }

    #[test]
    fn a_wire_that_collapses_on_insert_is_warned_about_but_not_charged_as_an_error() {
        // `SesReader.processWireScope` calls the `void` `insert_trace` and increments
        // `wiresImported` unconditionally (`SesReader.java:293-297`), so a path that survives
        // `read_polygon_path_scope` and then collapses in `insert_trace_without_cleaning`
        // (`BasicBoard.java:244-255`) is invisible to upstream's error counter. Our writer can
        // emit one: §12.3's duplicate-corner elision runs in session units, and two corners a
        // session unit apart round back onto the same board coordinate. `Issue039-bug-design`
        // is the fixture where that happens, and charging it as an error used to fail the whole
        // board on the G2.1 gate.
        let mut design = super::super::import(
            b"(pcb test\n\
              (parser (string_quote \") (space_in_quoted_tokens on))\n\
              (resolution um 10)\n\
              (structure\n\
                (layer F.Cu (type signal))\n\
                (layer B.Cu (type signal))\n\
                (boundary (rect pcb 0 0 10000 10000))\n\
                (rule (width 200) (clearance 200))\n\
              )\n\
              (library)\n\
              (network (net N (pins )))\n\
              (wiring)\n\
            )",
        )
        .expect("synthetic board should import");
        let library = SesLibrary::from_design(&design);
        let target = SesTarget::for_design(&mut design, &library);
        let summary = read_session(
            b"(session \"test.ses\"\n\
              (routes (network_out (net N (wire (path F.Cu 100 500 500 500 500)))))\n\
            )",
            target,
        )
        .expect("a well-formed session should parse");
        assert_eq!(summary.errors_encountered, 0, "{:?}", summary.warnings);
        assert_eq!(summary.wires_imported, 1);
        assert!(
            summary
                .warnings
                .iter()
                .any(|warning| warning.contains("degenerate wire skipped")),
            "{:?}",
            summary.warnings
        );
    }

    #[test]
    fn an_unknown_net_is_counted_as_an_error_and_skipped() {
        let mut design = empty_design();
        let library = SesLibrary::from_design(&design);
        let target = SesTarget::for_design(&mut design, &library);
        let summary = read_session(
            b"(session \"test.ses\"\n\
              (routes (network_out (net GHOST (wire (path F.Cu 100 0 0 100 100)))))\n\
            )",
            target,
        )
        .expect("a well-formed session should parse");
        assert_eq!(summary.errors_encountered, 1);
        assert_eq!(summary.wires_imported, 0);
    }
}
