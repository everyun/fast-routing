//! The scope readers: DSN token stream → [`DsnAst`].
//!
//! One method per upstream `readScope`, in upstream's control flow — including the
//! `prevToken == OPEN_BRACKET` idiom, which is what makes an unknown token *inside* a scope
//! harmless while an unknown token *after* a `(` opens a scope to skip.
//!
//! Nothing here touches a board. The two pieces of genuinely cross-scope state upstream threads
//! through `ReadScopeParameter` and that cannot be deferred live on [`Reader`]:
//!
//! * `layer_structure` — shape scopes resolve layer names to indices *while the structure scope
//!   is still being read*, so it is materialized on demand from the layers seen so far. That
//!   on-demand materialization is also the trigger for the `autoroute_settings` guard bug
//!   ([`Reader::read_structure_scope`]).
//! * `string_quote` — `Structure.setClearanceRule` re-parses quoted class-pair names by splitting
//!   on it, so it must be captured before the structure scope is built.

use super::ast::*;
use super::lex::{Kw, LexState, Lexer, Token};

/// Whether the read reached a usable AST.
#[derive(Debug, Clone, PartialEq)]
pub enum ReadOutcome {
    /// The file parsed. `warnings` may still be non-empty.
    Ok(Box<DsnAst>),
    /// The header was not `(pcb <name>`.
    NotADsnFile,
    /// A scope reader gave up. Upstream maps this onto `BoardReadResult.ParseError`.
    ParseError(String),
}

/// The reader's cross-scope state.
pub struct Reader<'a> {
    lexer: Lexer<'a>,
    ast: DsnAst,
    warnings: Vec<String>,
    /// `boardConstructionInfo.layerInfo` — the layers that passed `(type …)` validation.
    layer_info: Vec<DsnLayer>,
    /// `scopeParameter.layerStructure`, materialized on demand. `None` until the first shape or
    /// keepout scope needs it.
    layer_structure: Option<DsnLayerStructure>,
}

impl<'a> Reader<'a> {
    /// Parses a whole DSN file.
    pub fn read(src: &'a [u8]) -> (ReadOutcome, Vec<String>) {
        let mut reader = Reader {
            lexer: Lexer::new(src),
            ast: DsnAst {
                string_quote: "\"".to_string(),
                generated_by_host: true,
                unit: Unit::Mil,
                resolution: 100,
                ..DsnAst::default()
            },
            warnings: Vec::new(),
            layer_info: Vec::new(),
            layer_structure: None,
        };
        match reader.read_header() {
            Ok(name) => reader.ast.design_name = name,
            Err(outcome) => return (outcome, reader.warnings),
        }
        reader.read_scope_generic();
        (ReadOutcome::Ok(Box::new(reader.ast)), reader.warnings)
    }

    /// The three-token header check from `DsnReader.readBoard` (`DsnReader.java:80-107`). The
    /// `NAME` switch after `pcb` is what lets a numeric design name through.
    fn read_header(&mut self) -> Result<String, ReadOutcome> {
        if self.lexer.next_token() != Some(Token::Open) {
            return Err(ReadOutcome::NotADsnFile);
        }
        if self.lexer.next_token() != Some(Token::Kw(Kw::PcbScope)) {
            return Err(ReadOutcome::NotADsnFile);
        }
        self.lexer.begin(LexState::Name);
        let name = match self.lexer.next_token() {
            Some(token) => token.as_java_string().unwrap_or_default().to_string(),
            None => return Err(ReadOutcome::NotADsnFile),
        };
        Ok(name)
    }

    fn warn(&mut self, message: impl Into<String>) {
        self.warnings.push(message.into());
    }

    /// Upstream `ScopeKeyword.skipScope`: bracket counting with `yybegin(NAME)` before every
    /// token, so nothing inside a skipped scope is interpreted.
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

    /// Upstream `ScopeKeyword.readScope` — the generic driver used for `(pcb …)` and
    /// `(placement …)`.
    fn read_scope_generic(&mut self) -> bool {
        let mut next: Option<Token> = None;
        loop {
            let prev = next;
            next = self.lexer.next_token();
            match &next {
                None => return true,
                Some(Token::Close) => return true,
                _ => {}
            }
            if prev == Some(Token::Open) {
                match &next {
                    Some(Token::Kw(kw)) if is_scope_keyword(*kw) => {
                        if !self.dispatch_scope(*kw) {
                            return false;
                        }
                    }
                    _ => self.skip_scope(),
                }
            }
        }
    }

    fn dispatch_scope(&mut self, kw: Kw) -> bool {
        match kw {
            Kw::ParserScope => self.read_parser_scope(),
            Kw::ResolutionScope => self.read_resolution_scope(),
            Kw::StructureScope => self.read_structure_scope(),
            Kw::PlacementScope => self.read_scope_generic(),
            Kw::ComponentScope => self.read_component_scope(),
            Kw::PlaceControl => self.read_place_control_scope(),
            Kw::LibraryScope => self.read_library_scope(),
            Kw::NetworkScope => self.read_network_scope(),
            Kw::WiringScope => self.read_wiring_scope(),
            // `part_library` (gate/pin swap) is parsed and discarded: `Network.insertLogicalParts`
            // only affects interactive pin swapping, which this port does not have.
            Kw::PartLibraryScope | Kw::PcbScope | Kw::PlaneScope => {
                self.skip_scope();
                true
            }
            _ => {
                self.skip_scope();
                true
            }
        }
    }

    // ---------------------------------------------------------------------------------------
    // parser / resolution
    // ---------------------------------------------------------------------------------------

    fn read_parser_scope(&mut self) -> bool {
        let mut next: Option<Token> = None;
        loop {
            let prev = next;
            next = self.lexer.next_token();
            match &next {
                None => {
                    self.warn("Parser.read_scope: unexpected end of file");
                    return false;
                }
                Some(Token::Close) => return true,
                _ => {}
            }
            if prev != Some(Token::Open) {
                continue;
            }
            match &next {
                Some(Token::Kw(Kw::StringQuote)) => {
                    // The scanner is already in IGNORE_QUOTE, so the quote character itself
                    // arrives as an ordinary identifier.
                    if let Some(token) = self.lexer.next_token() {
                        if let Some(text) = token.as_java_string() {
                            self.ast.string_quote = text.to_string();
                        }
                    }
                    if !self.lexer.next_closing_bracket() {
                        self.warn("Parser.read_scope: closing bracket expected after string_quote");
                    }
                }
                Some(Token::Kw(Kw::HostCad)) => {
                    self.ast.host_cad = self.read_string_scope();
                }
                Some(Token::Kw(Kw::HostVersion)) => {
                    self.ast.host_version = self.read_string_scope();
                }
                Some(Token::Kw(Kw::Constant)) => {
                    let first = self.lexer.next_string(true, b' ');
                    let second = self.lexer.next_string(true, b' ');
                    if !self.lexer.next_closing_bracket() {
                        self.warn("Parser.read_scope: closing bracket expected after constant");
                    }
                    self.ast.constants.push((first, second));
                }
                Some(Token::Kw(Kw::GeneratedByFreeroute)) => {
                    self.ast.generated_by_host = false;
                    self.skip_scope();
                }
                _ => self.skip_scope(),
            }
        }
    }

    fn read_resolution_scope(&mut self) -> bool {
        let unit_token = self.lexer.next_token();
        let Some(unit_text) = unit_token.as_ref().and_then(Token::as_java_string) else {
            self.warn("Resolution.read_scope: string expected");
            return false;
        };
        let Some(unit) = Unit::from_dsn(unit_text) else {
            self.warn("Resolution.read_scope: unit mil, inch, mm or um expected");
            return false;
        };
        self.ast.unit = unit;
        let Some(resolution) = self.lexer.next_token().as_ref().and_then(Token::as_int) else {
            self.warn("Resolution.read_scope: integer expected");
            return false;
        };
        self.ast.resolution = resolution as i32;
        if !self.lexer.next_closing_bracket() {
            self.warn("Resolution.read_scope: closing bracket expected");
            return false;
        }
        true
    }

    // ---------------------------------------------------------------------------------------
    // small helpers shared by many scopes
    // ---------------------------------------------------------------------------------------

    /// Upstream `DsnFile.readStringScope`: one bypass string, then consume tokens until the
    /// closing bracket.
    fn read_string_scope(&mut self) -> Option<String> {
        let result = self.lexer.next_string(true, b' ');
        let mut token = self.lexer.next_token();
        while token.is_some() && token != Some(Token::Close) {
            token = self.lexer.next_token();
        }
        Some(result)
    }

    /// Upstream `DsnFile.readOnOffScope`.
    fn read_on_off_scope(&mut self) -> bool {
        let token = self.lexer.next_token();
        let result = token == Some(Token::Kw(Kw::On));
        if !result && token != Some(Token::Kw(Kw::Off)) {
            self.warn("DsnFile.read_boolean: on or off expected");
        }
        self.skip_scope();
        result
    }

    /// Upstream `DsnFile.readStringListScope`.
    fn read_string_list_scope(&mut self) -> Vec<String> {
        let result = self.lexer.next_string_list(b' ');
        if !self.lexer.next_closing_bracket() {
            self.warn("DsnFile.read_string_list_scope: closing bracket expected");
        }
        result
    }

    // ---------------------------------------------------------------------------------------
    // shapes
    // ---------------------------------------------------------------------------------------

    /// Upstream `Shape.getLayer`. `structure_layers` is `layerStructure != null`: the boundary
    /// scope passes `null`, so only the two pseudo layers are legal there.
    fn get_layer(&self, name: &str, structure_layers: bool) -> Option<DsnLayer> {
        if name == "pcb" {
            return Some(DsnLayer::pcb());
        }
        if name == "signal" {
            return Some(DsnLayer::signal());
        }
        if !structure_layers {
            return None;
        }
        let layers = self.layer_structure.as_ref()?;
        let index = layers.get_no(name);
        if index < 0 || index as usize >= layers.layers.len() {
            return None;
        }
        Some(layers.layers[index as usize].clone())
    }

    /// Upstream `Shape.readScope`: overread an optional `(`, then dispatch on the shape keyword.
    fn read_shape_scope(&mut self, structure_layers: bool) -> Option<DsnShape> {
        let mut token = self.lexer.next_token()?;
        if token == Token::Open {
            token = self.lexer.next_token()?;
        }
        self.read_shape_from_keyword(&token, structure_layers)
    }

    fn read_shape_from_keyword(
        &mut self,
        token: &Token,
        structure_layers: bool,
    ) -> Option<DsnShape> {
        match token {
            Token::Kw(Kw::Rectangle) => self.read_rectangle_scope(structure_layers),
            Token::Kw(Kw::Polygon) => self.read_polygon_scope(structure_layers),
            Token::Kw(Kw::Circle) => self.read_circle_scope(structure_layers),
            Token::Kw(Kw::PolygonPath) => self.read_polygon_path_scope(structure_layers),
            Token::Kw(Kw::PolylinePath) => self.read_polyline_path_scope(structure_layers),
            _ => {
                self.skip_scope();
                None
            }
        }
    }

    fn read_rectangle_scope(&mut self, structure_layers: bool) -> Option<DsnShape> {
        // `nextString()` here, not `nextString(true)`: newlines are *not* skipped, exactly as
        // upstream. A rectangle whose layer name sits on the next line does not parse there
        // either.
        let layer_name = self.lexer.next_string_default();
        let mut layer = self.get_layer(&layer_name, structure_layers);
        if layer.is_none() {
            // Upstream retries with the literal name "signal", which always resolves.
            layer = self.get_layer("signal", structure_layers);
        }
        let mut coor = [0.0f64; 4];
        for slot in coor.iter_mut() {
            match self.lexer.next_token().as_ref().and_then(Token::as_number) {
                Some(value) => *slot = value,
                None => {
                    self.warn("Shape.read_rectangle_scope: number expected");
                    return None;
                }
            }
        }
        if !self.lexer.next_closing_bracket() {
            self.warn("Shape.read_rectangle_scope: ) expected");
            return None;
        }
        Some(DsnShape::Rect {
            layer: layer?,
            coor,
        })
    }

    /// Upstream `Shape.readPolygonScope` — the one shape reader that resolves its layer through
    /// the DFA (`LAYER_NAME` state) instead of the bypass string reader.
    fn read_polygon_scope(&mut self, structure_layers: bool) -> Option<DsnShape> {
        let token = self.lexer.next_token()?;
        let mut layer_ok = true;
        let layer = match &token {
            Token::Kw(Kw::PcbScope) => Some(DsnLayer::pcb()),
            Token::Kw(Kw::Signal) => Some(DsnLayer::signal()),
            _ => {
                if !structure_layers {
                    self.warn("Shape.read_polygon_scope: only layer types pcb or signal expected");
                    return None;
                }
                let Some(name) = token.as_java_string().map(str::to_string) else {
                    self.warn("Shape.read_polygon_scope: layer name string expected");
                    return None;
                };
                if !token.is_string() {
                    self.warn("Shape.read_polygon_scope: layer name string expected");
                    return None;
                }
                match self.get_layer_by_index(&name) {
                    Some(layer) => Some(layer),
                    None => {
                        self.warn(format!(
                            "Shape.read_polygon_scope: layer name '{name}' not found in layer structure"
                        ));
                        layer_ok = false;
                        None
                    }
                }
            }
        };
        // Overread the aperture width.
        let _ = self.lexer.next_token();
        let mut coor = Vec::new();
        loop {
            let mut token = self.lexer.next_token();
            match token {
                None => {
                    self.warn("Shape.read_polygon_scope: unexpected end of file");
                    return None;
                }
                Some(Token::Open) => {
                    self.skip_scope();
                    token = self.lexer.next_token();
                }
                _ => {}
            }
            match token {
                Some(Token::Close) | None => break,
                Some(ref value) => match value.as_number() {
                    Some(number) => coor.push(number),
                    None => {
                        self.warn("Shape.read_polygon_scope: number expected");
                        return None;
                    }
                },
            }
        }
        if !layer_ok {
            return None;
        }
        Some(DsnShape::Polygon {
            layer: layer?,
            coor,
        })
    }

    /// The `layerStructure.getNo` half of `Shape.getLayer`, without the pseudo-layer branch.
    fn get_layer_by_index(&self, name: &str) -> Option<DsnLayer> {
        let layers = self.layer_structure.as_ref()?;
        let index = layers.get_no(name);
        if index < 0 || index as usize >= layers.layers.len() {
            return None;
        }
        Some(layers.layers[index as usize].clone())
    }

    fn read_circle_scope(&mut self, structure_layers: bool) -> Option<DsnShape> {
        let layer_name = self.lexer.next_string_default();
        let layer = self.get_layer(&layer_name, structure_layers);
        if layer.is_none() {
            self.warn(format!(
                "Shape.read_circle_scope: layer with name '{layer_name}' not found"
            ));
        }
        let mut coor = [0.0f64; 3];
        let mut index = 0usize;
        loop {
            let token = self.lexer.next_token();
            match token {
                Some(Token::Close) => break,
                None => {
                    self.warn("Shape.read_circle_scope: unexpected end of file");
                    return None;
                }
                Some(ref value) => {
                    if index > 2 {
                        self.warn("Shape.read_circle_scope: closed bracket expected");
                        return None;
                    }
                    match value.as_number() {
                        Some(number) => coor[index] = number,
                        None => {
                            self.warn("Shape.read_circle_scope: number expected");
                            return None;
                        }
                    }
                    index += 1;
                }
            }
        }
        Some(DsnShape::Circle {
            layer: layer?,
            coor,
        })
    }

    fn read_polygon_path_scope(&mut self, structure_layers: bool) -> Option<DsnShape> {
        let layer_name = self.lexer.next_string_default();
        let layer = self.get_layer(&layer_name, structure_layers);
        let mut values = Vec::new();
        loop {
            let mut token = self.lexer.next_token();
            if token == Some(Token::Open) {
                self.skip_scope();
                token = self.lexer.next_token();
            }
            match token {
                Some(Token::Close) | None => break,
                Some(ref value) => values.push(value.clone()),
            }
        }
        // width + at least two coordinate pairs.
        if values.len() < 5 {
            return None;
        }
        let layer = layer?;
        let Some(width) = values[0].as_number() else {
            self.warn("Shape.read_polygon_path_scope: number expected");
            return None;
        };
        let mut coor = Vec::with_capacity(values.len() - 1);
        for value in &values[1..] {
            match value.as_number() {
                Some(number) => coor.push(number),
                None => {
                    self.warn("Shape.read_polygon_path_scope: number expected");
                    return None;
                }
            }
        }
        Some(DsnShape::PolygonPath { layer, width, coor })
    }

    fn read_polyline_path_scope(&mut self, structure_layers: bool) -> Option<DsnShape> {
        let layer_name = self.lexer.next_string_default();
        let layer = self.get_layer(&layer_name, structure_layers);
        let mut values = Vec::new();
        loop {
            match self.lexer.next_token() {
                Some(Token::Close) | None => break,
                Some(value) => values.push(value),
            }
        }
        if values.len() < 5 {
            self.warn("PolylinePath.read_scope: too few numbers in scope");
            return None;
        }
        let Some(width) = values[0].as_number() else {
            self.warn("PolylinePath.read_scope: number expected");
            return None;
        };
        let mut coor = Vec::with_capacity(values.len() - 1);
        for value in &values[1..] {
            match value.as_number() {
                Some(number) => coor.push(number),
                None => {
                    self.warn("PolylinePath.read_scope: number expected");
                    return None;
                }
            }
        }
        Some(DsnShape::PolylinePath {
            layer: layer?,
            width,
            coor,
        })
    }

    /// Upstream `Shape.readAreaScope`: an optional name, one border shape, then `(window …)`
    /// holes and an optional `(clearance_class …)`.
    ///
    /// `skip_window_scopes` is set only for Cadence Allegro power planes, where the exported
    /// per-pin cutouts make the convex decomposition explode.
    fn read_area_scope(
        &mut self,
        structure_layers: bool,
        skip_window_scopes: bool,
    ) -> Option<DsnArea> {
        let mut shapes = Vec::new();
        let mut clearance_class_name = None;
        let mut area_name = None;
        let token = self.lexer.next_token()?;
        if let Token::Str(name) = &token {
            self.lexer.set_scope_identifier(name);
            if !name.is_empty() {
                area_name = Some(name.clone());
            }
        }
        let border = self.read_shape_scope(structure_layers);
        let result_ok = border.is_some();
        if border.is_none() {
            self.warn("Shape.read_area_scope: could not read shape");
        }
        if let Some(shape) = border {
            shapes.push(shape);
        }
        let mut next: Option<Token> = None;
        loop {
            let prev = next;
            next = self.lexer.next_token();
            match &next {
                None => {
                    self.warn("Shape.read_area_scope: unexpected end of file");
                    return None;
                }
                Some(Token::Close) => break,
                _ => {}
            }
            if prev != Some(Token::Open) {
                continue;
            }
            match &next {
                Some(Token::Kw(Kw::Window)) if !skip_window_scopes => {
                    if let Some(hole) = self.read_shape_scope(structure_layers) {
                        shapes.push(hole);
                    }
                    if !self.lexer.next_closing_bracket() {
                        self.warn("Shape.read_area_scope: closed bracket expected");
                        return None;
                    }
                    next = Some(Token::Close);
                }
                Some(Token::Kw(Kw::ClearanceClass)) => {
                    clearance_class_name = self.read_string_scope();
                }
                _ => self.skip_scope(),
            }
        }
        if !result_ok {
            return None;
        }
        Some(DsnArea {
            name: area_name,
            shapes,
            clearance_class_name,
        })
    }

    // ---------------------------------------------------------------------------------------
    // rules
    // ---------------------------------------------------------------------------------------

    /// Upstream `Rule.readScope`: only `(width …)` and `(clearance …)` are understood.
    fn read_rule_scope(&mut self) -> Vec<DsnRule> {
        let mut result = Vec::new();
        let mut current: Option<Token> = None;
        loop {
            let prev = current;
            current = self.lexer.next_token();
            match &current {
                None => {
                    self.warn("Rule.read_scope: unexpected end of file");
                    return result;
                }
                Some(Token::Close) => return result,
                _ => {}
            }
            if prev != Some(Token::Open) {
                continue;
            }
            match &current {
                Some(Token::Kw(Kw::Width)) => {
                    if let Some(rule) = self.read_width_rule() {
                        result.push(rule);
                    }
                }
                Some(Token::Kw(Kw::Clearance)) => {
                    if let Some(rule) = self.read_clearance_rule() {
                        result.push(rule);
                    }
                }
                _ => self.skip_scope(),
            }
        }
    }

    fn read_width_rule(&mut self) -> Option<DsnRule> {
        let value = self.lexer.next_double()?;
        if !self.lexer.next_closing_bracket() {
            return None;
        }
        Some(DsnRule::Width(value))
    }

    /// Upstream `Rule.readClearanceRule`.
    ///
    /// The `(type …)` list is split on `-`, never on `_`, so `(type smd-via)` yields the
    /// two-element list `["smd", "via"]` while `(type smd_via)` yields `["smd_via"]`. Both
    /// encodings occur in the wild and take different branches in `build.rs`.
    fn read_clearance_rule(&mut self) -> Option<DsnRule> {
        let value = self.lexer.next_double()?;
        let mut class_pairs = Vec::new();
        let token = self.lexer.next_token();
        if token != Some(Token::Close) {
            if token != Some(Token::Open) {
                self.warn("Rule.read_clearance_rule: ( expected");
                return None;
            }
            if self.lexer.next_token() != Some(Token::Kw(Kw::Type)) {
                self.warn("Rule.read_clearance_rule: type expected");
                return None;
            }
            class_pairs = self
                .lexer
                .next_string_list(super::CLASS_CLEARANCE_SEPARATOR_BYTE);
            if !self.lexer.next_closing_bracket() || !self.lexer.next_closing_bracket() {
                self.warn("Rule.read_clearance_rule: closing bracket expected");
                return None;
            }
        }
        Some(DsnRule::Clearance { value, class_pairs })
    }

    /// Upstream `Rule.readLayerRuleScope`: layer names in `LAYER_NAME` mode until the first `(`,
    /// then one or more `(rule …)` blocks.
    fn read_layer_rule_scope(&mut self) -> Option<DsnLayerRule> {
        let mut layer_names = Vec::new();
        loop {
            self.lexer.begin(LexState::LayerName);
            let token = self.lexer.next_token()?;
            if token == Token::Open {
                break;
            }
            match token.as_java_string() {
                Some(name) if token.is_string() => layer_names.push(name.to_string()),
                _ => {
                    self.warn("Rule.read_layer_rule_scope: string expected");
                    return None;
                }
            }
        }
        let mut rules = Vec::new();
        loop {
            let token = self.lexer.next_token()?;
            if token == Token::Close {
                break;
            }
            if token != Token::Kw(Kw::Rule) {
                self.warn("Rule.read_layer_rule_scope: rule expected");
                return None;
            }
            rules.extend(self.read_rule_scope());
        }
        Some(DsnLayerRule { layer_names, rules })
    }

    // ---------------------------------------------------------------------------------------
    // structure
    // ---------------------------------------------------------------------------------------

    fn materialize_layer_structure(&mut self) {
        self.layer_structure = Some(DsnLayerStructure {
            layers: self.layer_info.clone(),
        });
    }

    fn read_structure_scope(&mut self) -> bool {
        let mut next: Option<Token> = None;
        loop {
            let prev = next;
            next = self.lexer.next_token();
            match &next {
                None => {
                    self.warn("Structure.read_scope: unexpected end of file");
                    return false;
                }
                Some(Token::Close) => break,
                _ => {}
            }
            if prev != Some(Token::Open) {
                continue;
            }
            match &next {
                Some(Token::Kw(Kw::Boundary)) => self.read_boundary_scope(),
                Some(Token::Kw(Kw::Layer)) => {
                    if !self.read_layer_scope() {
                        return false;
                    }
                    if self.layer_structure.is_some() {
                        // Upstream rebuilds the structure after every additional layer, which is
                        // also what keeps the `autoroute_settings` guard below latched.
                        self.materialize_layer_structure();
                    }
                }
                Some(Token::Kw(Kw::Via)) => {
                    self.ast.structure.via_padstack_names = self.read_via_padstacks();
                }
                Some(Token::Kw(Kw::Rule)) => {
                    let rules = self.read_rule_scope();
                    self.ast.structure.default_rules.extend(rules);
                }
                Some(Token::Kw(Kw::Keepout)) => {
                    if self.layer_structure.is_none() {
                        self.materialize_layer_structure();
                    }
                    if let Some(area) = self.read_area_scope(true, false) {
                        self.ast.structure.keepouts.push(area);
                    }
                }
                Some(Token::Kw(Kw::ViaKeepout)) => {
                    if self.layer_structure.is_none() {
                        self.materialize_layer_structure();
                    }
                    if let Some(area) = self.read_area_scope(true, false) {
                        self.ast.structure.via_keepouts.push(area);
                    }
                }
                Some(Token::Kw(Kw::PlaceKeepout)) => {
                    if self.layer_structure.is_none() {
                        self.materialize_layer_structure();
                    }
                    if let Some(area) = self.read_area_scope(true, false) {
                        self.ast.structure.place_keepouts.push(area);
                    }
                }
                Some(Token::Kw(Kw::PlaneScope)) => {
                    if self.layer_structure.is_none() {
                        self.materialize_layer_structure();
                    }
                    self.read_plane_scope();
                }
                Some(Token::Kw(Kw::AutorouteSettings)) => {
                    // **The guard bug, reproduced.** If any keepout, via keepout, place keepout
                    // or plane scope came first — or if a second `(layer …)` scope did — the
                    // layer structure is already materialized and this scope is neither parsed
                    // *nor skipped*. Its contents then fall through to this loop as unknown
                    // scopes and are skipped one by one, so parsing still terminates and the
                    // settings are silently dropped (`Structure.java:1007`). Freerouting's own
                    // writer emits `autoroute_settings` before the keepouts, so its round trip
                    // works; third-party files mostly hit the bug, which is in turn what makes
                    // `adjustPlaneAutorouteSettings` run on them.
                    if self.layer_structure.is_none() {
                        self.materialize_layer_structure();
                        self.ast.structure.autoroute_settings = self.read_autoroute_settings();
                    }
                }
                Some(Token::Kw(Kw::Control)) => {
                    if !self.read_control_scope() {
                        return false;
                    }
                }
                Some(Token::Kw(Kw::FlipStyle)) => {
                    self.ast.structure.flip_style_rotate_first =
                        self.read_flip_style_rotate_first();
                }
                Some(Token::Kw(Kw::SnapAngle)) => {
                    if let Some(angle) = self.read_snap_angle() {
                        self.ast.structure.snap_angle = Some(angle);
                    }
                }
                _ => self.skip_scope(),
            }
        }
        self.ast.structure.layers = self.layer_info.clone();
        // `Structure.createBoard` assigns the completed structure unconditionally, so every later
        // scope sees all layers even if a keepout materialized a partial one earlier.
        self.materialize_layer_structure();
        true
    }

    /// Upstream `Structure.readBoundaryScope` / `addBoundaryShape`.
    fn read_boundary_scope(&mut self) {
        let first = self.read_shape_scope(false);
        let mut next: Option<Token> = None;
        loop {
            let prev = next;
            next = self.lexer.next_token();
            match &next {
                None => break,
                Some(Token::Close) => break,
                _ => {}
            }
            if prev == Some(Token::Open) {
                if next == Some(Token::Kw(Kw::ClearanceClass)) {
                    self.ast.structure.outline_clearance_class_name = self.read_string_scope();
                } else if let Some(token) = next.clone() {
                    let shape = self.read_shape_from_keyword(&token, false);
                    self.add_boundary_shape(shape);
                }
            }
        }
        match first {
            None => self.warn("Structure.read_boundary_scope: shape is null"),
            Some(shape) => self.add_boundary_shape(Some(shape)),
        }
    }

    fn add_boundary_shape(&mut self, shape: Option<DsnShape>) {
        let Some(shape) = shape else { return };
        if shape.is_path() {
            self.ast.structure.outline_shapes.push(shape);
            return;
        }
        if shape.layer().is_pcb_pseudo() {
            if self.ast.structure.bounding_shape.is_none() {
                self.ast.structure.bounding_shape = Some(shape);
            } else {
                self.ast.structure.outline_shapes.push(shape);
            }
        } else if shape.layer().is_signal_pseudo() {
            self.ast.structure.outline_shapes.push(shape);
        } else {
            self.warn("Structure.add_boundary_shape: unexpected layer at boundary");
        }
    }

    /// Upstream `Structure.readLayerScope`.
    ///
    /// A layer whose `(type …)` is neither `signal`, `power` nor the *string* `jumper` is dropped
    /// entirely and does not consume an index, so a rejected layer shifts every later layer down.
    fn read_layer_scope(&mut self) -> bool {
        let mut layer_ok = true;
        let mut is_signal = true;
        let layer_name = self.lexer.next_string_default();
        let mut net_names = Vec::new();
        let mut token = self.lexer.next_token();
        while token != Some(Token::Close) {
            if token != Some(Token::Open) {
                self.warn("Structure.read_layer_scope: ( expected");
                return false;
            }
            token = self.lexer.next_token();
            match &token {
                Some(Token::Kw(Kw::Type)) => {
                    let type_token = self.lexer.next_token();
                    match &type_token {
                        Some(Token::Kw(Kw::Power)) => is_signal = false,
                        Some(Token::Kw(Kw::Signal)) => {}
                        other => {
                            // `Keyword.JUMPER` is declared but never produced by the lexer, so
                            // `(type jumper)` arrives as a string and upstream compares text.
                            let is_jumper = other
                                .as_ref()
                                .and_then(Token::as_java_string)
                                .is_some_and(|text| text == "jumper");
                            if !is_jumper {
                                self.warn(format!(
                                    "Structure.read_layer_scope: the layer '{layer_name}' has an unknown layer type"
                                ));
                                layer_ok = false;
                            }
                        }
                    }
                    if self.lexer.next_token() != Some(Token::Close) {
                        self.warn("Structure.read_layer_scope: ) expected");
                        return false;
                    }
                }
                Some(Token::Kw(Kw::Rule)) => {
                    let rules = self.read_rule_scope();
                    self.ast.structure.layer_dependent_rules.push(DsnLayerRule {
                        layer_names: vec![layer_name.clone()],
                        rules,
                    });
                }
                Some(Token::Kw(Kw::UseNet)) => loop {
                    self.lexer.begin(LexState::Name);
                    let net_token = self.lexer.next_token();
                    match &net_token {
                        Some(Token::Close) | None => break,
                        Some(value) if value.is_string() => {
                            net_names.push(value.as_java_string().unwrap_or_default().to_string());
                        }
                        _ => self.warn("Structure.read_layer_scope: string expected"),
                    }
                },
                _ => self.skip_scope(),
            }
            token = self.lexer.next_token();
        }
        if self.layer_info.len() >= 256 {
            self.warn("Structure.read_layer_scope: max layer limit reached");
            return false;
        }
        if layer_ok {
            let no = self.layer_info.len() as i32;
            self.layer_info.push(DsnLayer {
                name: layer_name,
                no,
                is_signal,
                net_names,
            });
        }
        true
    }

    /// Upstream `Structure.readViaPadstacks`: names, with the `(spare …)` list appended at the
    /// end.
    fn read_via_padstacks(&mut self) -> Vec<String> {
        let mut normal = Vec::new();
        let mut spare = Vec::new();
        loop {
            let token = self.lexer.next_token();
            match &token {
                None | Some(Token::Close) => break,
                Some(Token::Open) => {
                    let inner = self.lexer.next_token();
                    if inner == Some(Token::Kw(Kw::Spare)) {
                        spare = self.read_via_padstacks();
                    } else {
                        self.skip_scope();
                    }
                }
                Some(value) if value.is_string() => {
                    normal.push(value.as_java_string().unwrap_or_default().to_string());
                }
                _ => {
                    self.warn("Structure.read_via_padstack: String expected");
                    return Vec::new();
                }
            }
        }
        normal.extend(spare);
        normal
    }

    fn read_control_scope(&mut self) -> bool {
        let mut next: Option<Token> = None;
        loop {
            let prev = next;
            next = self.lexer.next_token();
            match &next {
                None => {
                    self.warn("Structure.read_control_scope: unexpected end of file");
                    return false;
                }
                Some(Token::Close) => return true,
                _ => {}
            }
            if prev == Some(Token::Open) {
                if next == Some(Token::Kw(Kw::ViaAtSmd)) {
                    self.ast.structure.via_at_smd_allowed = self.read_on_off_scope();
                } else {
                    self.skip_scope();
                }
            }
        }
    }

    /// Upstream `PlaceControl.readFlipStyleRotateFirst`.
    fn read_flip_style_rotate_first(&mut self) -> bool {
        let mut result = false;
        loop {
            match self.lexer.next_token() {
                None | Some(Token::Close) => return result,
                Some(Token::Kw(Kw::RotateFirst)) => result = true,
                _ => {}
            }
        }
    }

    /// Upstream `Structure.readSnapAngle`, encoded as the `AngleRestriction` ordinal
    /// (`0 = none`, `1 = fortyfive`, `2 = ninety`) — the declaration order of
    /// `board/AngleRestriction.java:8` (`NONE, FORTYFIVE_DEGREE, NINETY_DEGREE`).
    fn read_snap_angle(&mut self) -> Option<i32> {
        let token = self.lexer.next_token()?;
        let angle = match token {
            Token::Kw(Kw::NinetyDegree) => 2,
            Token::Kw(Kw::FortyfiveDegree) => 1,
            Token::Kw(Kw::None) => 0,
            _ => {
                self.warn("Structure.read_snap_angle_scope: unexpected token");
                return None;
            }
        };
        if !self.lexer.next_closing_bracket() {
            self.warn("Structure.read_snap_angle: closing bracket expected");
            return None;
        }
        Some(angle)
    }

    /// Upstream `Plane.readScope`. Allegro's per-pin plane cutouts are skipped, as upstream does,
    /// because splitting them into convex pieces is what makes those boards unusable.
    fn read_plane_scope(&mut self) -> bool {
        let skip_windows = self
            .ast
            .host_cad
            .as_deref()
            .is_some_and(|host| host.eq_ignore_ascii_case("allegro"));
        let token = self.lexer.next_token();
        let Some(net_name) = token
            .as_ref()
            .and_then(Token::as_java_string)
            .filter(|_| token.as_ref().is_some_and(Token::is_string))
        else {
            self.warn("Plane.read_scope: String expected");
            return false;
        };
        let net_name = net_name.to_string();
        self.lexer.set_scope_identifier(&net_name);
        let Some(area) = self.read_area_scope(true, skip_windows) else {
            return false;
        };
        self.ast.structure.planes.push(DsnPlane { net_name, area });
        true
    }

    /// Upstream `AutorouteSettings.readScope`.
    ///
    /// Three ways this returns `None`, i.e. drops the whole scope — all of them upstream's:
    /// end of file inside the scope (`:31-34`), a `layer_rule` naming a layer the structure does
    /// not have (`:52-55` propagating `read_layer_rule`'s `null` at `:82-85`), and, one level up,
    /// the `layerStructure == null` guard that means this method is never called at all.
    ///
    /// `(start_pass_no …)` has **no branch upstream** — `Keyword.START_PASS_NO` exists but
    /// `read_scope` never tests for it, so the scope goes to `ScopeKeyword.skip_scope` and the
    /// value is discarded. Reading it as an integer here consumes exactly the same tokens.
    fn read_autoroute_settings(&mut self) -> Option<DsnAutorouteSettings> {
        let mut settings = DsnAutorouteSettings::default();
        let mut next: Option<Token> = None;
        loop {
            let prev = next;
            next = self.lexer.next_token();
            match &next {
                None => {
                    self.warn("AutorouteSettings.read_scope: unexpected end of file");
                    return None;
                }
                Some(Token::Close) => return Some(settings),
                _ => {}
            }
            if prev != Some(Token::Open) {
                continue;
            }
            match &next {
                Some(Token::Kw(Kw::Fanout)) => {
                    // Parsed and discarded upstream.
                    let _ = self.read_on_off_scope();
                }
                Some(Token::Kw(Kw::Autoroute)) => {
                    settings.run_router = Some(self.read_on_off_scope())
                }
                Some(Token::Kw(Kw::Postroute)) => {
                    settings.run_optimizer = Some(self.read_on_off_scope())
                }
                Some(Token::Kw(Kw::Vias)) => settings.vias_allowed = Some(self.read_on_off_scope()),
                Some(Token::Kw(Kw::ViaCosts)) => settings.via_costs = self.read_integer_scope(),
                Some(Token::Kw(Kw::PlaneViaCosts)) => {
                    settings.plane_via_costs = self.read_integer_scope()
                }
                Some(Token::Kw(Kw::StartRipupCosts)) => {
                    settings.start_ripup_costs = self.read_integer_scope()
                }
                Some(Token::Kw(Kw::StartPassNo)) => {
                    let _ = self.read_integer_scope();
                }
                Some(Token::Kw(Kw::LayerRule)) => {
                    // `null` from `read_layer_rule` aborts `read_scope` itself, *without* first
                    // skipping the rest of the rule: the remaining tokens fall out into the
                    // structure loop and are skipped there one scope at a time, exactly as the
                    // guard-bug path does.
                    settings
                        .layer_settings
                        .push(self.read_autoroute_layer_rule()?);
                }
                _ => self.skip_scope(),
            }
        }
    }

    fn read_integer_scope(&mut self) -> Option<i32> {
        let value = self.lexer.next_token().as_ref().and_then(Token::as_int);
        if !self.lexer.next_closing_bracket() {
            self.warn("DsnFile.read_integer_scope: closing bracket expected");
        }
        value.map(|v| v as i32)
    }

    fn read_float_scope(&mut self) -> Option<f64> {
        let value = self.lexer.next_token().as_ref().and_then(Token::as_number);
        if !self.lexer.next_closing_bracket() {
            self.warn("DsnFile.read_float_scope: closing bracket expected");
        }
        value
    }

    /// Upstream `AutorouteSettings.read_layer_rule`.
    ///
    /// The layer name is resolved here, not at build time, because upstream resolves it here and
    /// a name that resolves to `-1` is fatal to the *enclosing* scope, not just to this rule
    /// (`AutorouteSettings.java:82-85`). The caller turns that into a dropped
    /// `autoroute_settings`.
    fn read_autoroute_layer_rule(&mut self) -> Option<DsnLayerSetting> {
        let layer_name = self.lexer.next_string(true, b' ');
        let layer_no = self
            .layer_structure
            .as_ref()
            .map_or(-1, |layers| layers.get_no(&layer_name));
        let layer_count = self
            .layer_structure
            .as_ref()
            .map_or(0, |layers| layers.layers.len());
        if layer_no < 0 || layer_no as usize >= layer_count {
            self.warn("AutorouteSettings.read_layer_rule: layer not found");
            return None;
        }
        let mut setting = DsnLayerSetting {
            layer_name,
            layer_no: layer_no as usize,
            ..DsnLayerSetting::default()
        };
        let mut next: Option<Token> = None;
        loop {
            let prev = next;
            next = self.lexer.next_token();
            match &next {
                None => {
                    self.warn("AutorouteSettings.read_layer_rule: unexpected end of file");
                    return None;
                }
                Some(Token::Close) => return Some(setting),
                _ => {}
            }
            if prev != Some(Token::Open) {
                continue;
            }
            match &next {
                Some(Token::Kw(Kw::Active)) => setting.active = Some(self.read_on_off_scope()),
                Some(Token::Kw(Kw::PreferredDirection)) => {
                    let token = self.lexer.next_token();
                    setting.preferred_direction_is_horizontal =
                        Some(token == Some(Token::Kw(Kw::Horizontal)));
                    self.skip_scope();
                }
                Some(Token::Kw(Kw::PreferredDirectionTraceCosts)) => {
                    setting.preferred_direction_trace_costs = self.read_float_scope();
                }
                Some(Token::Kw(Kw::AgainstPreferredDirectionTraceCosts)) => {
                    setting.against_preferred_direction_trace_costs = self.read_float_scope();
                }
                _ => self.skip_scope(),
            }
        }
    }

    // ---------------------------------------------------------------------------------------
    // placement
    // ---------------------------------------------------------------------------------------

    fn read_place_control_scope(&mut self) -> bool {
        let mut next: Option<Token> = None;
        loop {
            let prev = next;
            next = self.lexer.next_token();
            match &next {
                None => return false,
                Some(Token::Close) => return true,
                _ => {}
            }
            if prev == Some(Token::Open)
                && next == Some(Token::Kw(Kw::FlipStyle))
                && self.read_flip_style_rotate_first()
            {
                self.ast.structure.flip_style_rotate_first = true;
            }
        }
    }

    /// Upstream `Component.readScope`.
    fn read_component_scope(&mut self) -> bool {
        let token = self.lexer.next_token();
        let Some(lib_name) = token
            .as_ref()
            .filter(|t| t.is_string())
            .and_then(Token::as_java_string)
        else {
            self.warn("Component.read_scope: component name expected");
            return false;
        };
        let mut placement = ComponentPlacement {
            lib_name: lib_name.to_string(),
            locations: Vec::new(),
        };
        let mut prev = token;
        let mut next = self.lexer.next_token();
        while next.is_some() && next != Some(Token::Close) {
            if prev == Some(Token::Open) && next == Some(Token::Kw(Kw::Place)) {
                match self.read_place_scope() {
                    Some(location) => placement.locations.push(location),
                    None => return false,
                }
            }
            prev = next;
            next = self.lexer.next_token();
        }
        self.ast.placements.push(placement);
        true
    }

    /// Upstream `Component.readPlaceScope`.
    fn read_place_scope(&mut self) -> Option<ComponentLocation> {
        let name = self.lexer.next_string(true, b' ');
        let mut coor = [0.0f64; 2];
        let mut placed = true;
        for slot in coor.iter_mut() {
            let token = self.lexer.next_token();
            match &token {
                Some(value) if value.as_number().is_some() => {
                    *slot = value.as_number().unwrap_or_default();
                }
                Some(Token::Close) => {
                    placed = false;
                    break;
                }
                _ => {
                    self.warn(
                        "Component.read_place_scope: number expected as the component location",
                    );
                    return None;
                }
            }
        }
        if !placed {
            return Some(ComponentLocation {
                name,
                coor: None,
                is_front: true,
                rotation: 0.0,
                position_fixed: false,
                pin_infos: Vec::new(),
                keepout_infos: Vec::new(),
                via_keepout_infos: Vec::new(),
                place_keepout_infos: Vec::new(),
                part_number: None,
            });
        }
        let side_token = self.lexer.next_token();
        let is_front = side_token != Some(Token::Kw(Kw::Back));
        if is_front && side_token != Some(Token::Kw(Kw::Front)) {
            self.warn("Component.read_place_scope: front or back expected");
        }
        let Some(rotation) = self.lexer.next_token().as_ref().and_then(Token::as_number) else {
            self.warn("Component.read_place_scope: number expected for the rotation");
            return None;
        };
        let mut position_fixed = false;
        let mut part_number = None;
        let mut pin_infos = Vec::new();
        let mut keepout_infos = Vec::new();
        let mut via_keepout_infos = Vec::new();
        let mut place_keepout_infos = Vec::new();
        let mut token = self.lexer.next_token();
        while token == Some(Token::Open) {
            token = self.lexer.next_token();
            match &token {
                Some(Token::Kw(Kw::LockType)) => position_fixed = self.read_lock_type(),
                Some(Token::Kw(Kw::Pin)) => pin_infos.push(self.read_item_clearance_info()?),
                Some(Token::Kw(Kw::Keepout)) => {
                    keepout_infos.push(self.read_item_clearance_info()?)
                }
                Some(Token::Kw(Kw::ViaKeepout)) => {
                    via_keepout_infos.push(self.read_item_clearance_info()?)
                }
                Some(Token::Kw(Kw::PlaceKeepout)) => {
                    place_keepout_infos.push(self.read_item_clearance_info()?)
                }
                // `Keyword.PN` is declared upstream but never produced by the lexer, so a part
                // number arrives as the *string* "PN". A reader that only compares against the
                // keyword enum silently drops every part number.
                Some(value)
                    if value
                        .as_java_string()
                        .is_some_and(|text| text.eq_ignore_ascii_case("PN")) =>
                {
                    part_number = self.read_string_scope();
                }
                _ => self.skip_scope(),
            }
            token = self.lexer.next_token();
        }
        if token != Some(Token::Close) {
            self.warn("Component.read_place_scope: ) expected");
            return None;
        }
        Some(ComponentLocation {
            name,
            coor: Some(coor),
            is_front,
            rotation,
            position_fixed,
            pin_infos,
            keepout_infos,
            via_keepout_infos,
            place_keepout_infos,
            part_number,
        })
    }

    fn read_lock_type(&mut self) -> bool {
        let mut result = false;
        loop {
            match self.lexer.next_token() {
                None | Some(Token::Close) => return result,
                Some(Token::Kw(Kw::Position)) => result = true,
                _ => {}
            }
        }
    }

    /// Upstream `Component.readItemClearanceInfo`, including the v2 robustness addition that
    /// accepts the raw strings `clearance_class` / `clearanceClass` as well as the keyword.
    fn read_item_clearance_info(&mut self) -> Option<ItemClearanceInfo> {
        self.lexer.begin(LexState::Name);
        let token = self.lexer.next_token();
        let Some(name) = token
            .as_ref()
            .filter(|t| t.is_string())
            .and_then(Token::as_java_string)
            .map(str::to_string)
        else {
            self.warn("Component.read_item_clearance_info: String expected");
            return None;
        };
        let mut clearance_class = None;
        let mut token = self.lexer.next_token();
        while token == Some(Token::Open) {
            token = self.lexer.next_token();
            let is_clearance_class = token == Some(Token::Kw(Kw::ClearanceClass))
                || token
                    .as_ref()
                    .and_then(Token::as_java_string)
                    .is_some_and(|text| {
                        text.eq_ignore_ascii_case("clearance_class")
                            || text.eq_ignore_ascii_case("clearanceClass")
                    });
            if is_clearance_class {
                clearance_class = self.read_string_scope();
            } else {
                self.skip_scope();
            }
            token = self.lexer.next_token();
        }
        if token != Some(Token::Close) {
            self.warn("Component.read_item_clearance_info: ) expected");
            return None;
        }
        let clearance_class = clearance_class?;
        Some(ItemClearanceInfo {
            name,
            clearance_class,
        })
    }

    // ---------------------------------------------------------------------------------------
    // library
    // ---------------------------------------------------------------------------------------

    fn read_library_scope(&mut self) -> bool {
        let mut next: Option<Token> = None;
        loop {
            let prev = next;
            next = self.lexer.next_token();
            match &next {
                None => {
                    self.warn("Library.read_scope: unexpected end of file");
                    return false;
                }
                Some(Token::Close) => return true,
                _ => {}
            }
            if prev != Some(Token::Open) {
                continue;
            }
            match &next {
                Some(Token::Kw(Kw::Padstack)) => {
                    if !self.read_padstack_scope() {
                        return false;
                    }
                }
                Some(Token::Kw(Kw::Image)) => match self.read_package_scope() {
                    Some(package) => self.ast.packages.push(package),
                    None => return false,
                },
                _ => self.skip_scope(),
            }
        }
    }

    /// Upstream `Library.readPadstackScope`.
    ///
    /// The `.<digits>` suffix is stripped here, at the *definition* site — one of the five places
    /// KiCad's disambiguating suffix has to be removed for the lookups to resolve.
    fn read_padstack_scope(&mut self) -> bool {
        let token = self.lexer.next_token();
        let Some(raw_name) = token
            .as_ref()
            .filter(|t| t.is_string())
            .and_then(Token::as_java_string)
        else {
            self.warn("Library.read_padstack_scope: unexpected padstack identifier");
            return false;
        };
        let name = super::strip_numeric_suffix(raw_name);
        self.lexer.set_scope_identifier(&name);
        let mut attach_allowed = true;
        let mut placed_absolute = false;
        let mut shapes = Vec::new();
        let mut next = token;
        while next != Some(Token::Close) {
            let prev = next;
            next = self.lexer.next_token();
            if next.is_none() {
                self.warn("Library.read_padstack_scope: unexpected end of file");
                return false;
            }
            if prev != Some(Token::Open) {
                continue;
            }
            match &next {
                Some(Token::Kw(Kw::Shape)) => {
                    if let Some(shape) = self.read_shape_scope(true) {
                        shapes.push(shape);
                    }
                    // KiCad's `(reduced …)` and `(connect …)` live here.
                    let mut inner = self.lexer.next_token();
                    while inner == Some(Token::Open) {
                        self.skip_scope();
                        inner = self.lexer.next_token();
                    }
                    if inner != Some(Token::Close) {
                        self.warn("Library.read_padstack_scope: closing bracket expected");
                        return false;
                    }
                }
                Some(Token::Kw(Kw::Attach)) => attach_allowed = self.read_on_off_scope(),
                Some(Token::Kw(Kw::Absolute)) => placed_absolute = self.read_on_off_scope(),
                _ => self.skip_scope(),
            }
        }
        // First definition wins, exactly as upstream: a duplicate is parsed and discarded.
        if self.ast.padstacks.iter().any(|p| p.name == name) {
            return true;
        }
        if shapes.is_empty() {
            self.warn(format!(
                "Library.read_padstack_scope: shape not found for padstack with name '{name}'"
            ));
            return true;
        }
        self.ast.padstacks.push(DsnPadstack {
            name,
            shapes,
            attach_allowed,
            placed_absolute,
        });
        true
    }

    /// Upstream `Package.readScope`.
    fn read_package_scope(&mut self) -> Option<DsnPackage> {
        let token = self.lexer.next_token();
        let Some(name) = token
            .as_ref()
            .filter(|t| t.is_string())
            .and_then(Token::as_java_string)
            .map(str::to_string)
        else {
            self.warn("Package.read_scope: String expected");
            return None;
        };
        self.lexer.set_scope_identifier(&name);
        let mut package = DsnPackage {
            name,
            pins: Vec::new(),
            outlines: Vec::new(),
            keepouts: Vec::new(),
            via_keepouts: Vec::new(),
            place_keepouts: Vec::new(),
            is_front: true,
        };
        let mut next = token;
        loop {
            let prev = next;
            next = self.lexer.next_token();
            match &next {
                None => {
                    self.warn("Package.read_scope: unexpected end of file");
                    return None;
                }
                Some(Token::Close) => break,
                _ => {}
            }
            if prev != Some(Token::Open) {
                continue;
            }
            match &next {
                Some(Token::Kw(Kw::Pin)) => package.pins.push(self.read_pin_info()?),
                Some(Token::Kw(Kw::Side)) => {
                    // `readPlacementSide` returns `nextToken != BACK`, so a typo means front.
                    let side = self.lexer.next_token();
                    package.is_front = side != Some(Token::Kw(Kw::Back));
                    self.skip_scope();
                }
                Some(Token::Kw(Kw::Outline)) => {
                    if let Some(shape) = self.read_shape_scope(true) {
                        package.outlines.push(shape);
                    }
                    next = self.lexer.next_token();
                    if next != Some(Token::Close) {
                        self.warn("Package.read_scope: closed bracket expected");
                        return None;
                    }
                }
                Some(Token::Kw(Kw::Keepout)) => match self.read_area_scope(true, false) {
                    Some(area) => package.keepouts.push(area),
                    None => self.warn("Package.read_scope: could not read keepout area"),
                },
                Some(Token::Kw(Kw::ViaKeepout)) => {
                    if let Some(area) = self.read_area_scope(true, false) {
                        package.via_keepouts.push(area);
                    }
                }
                Some(Token::Kw(Kw::PlaceKeepout)) => {
                    if let Some(area) = self.read_area_scope(true, false) {
                        package.place_keepouts.push(area);
                    }
                }
                _ => self.skip_scope(),
            }
        }
        Some(package)
    }

    /// Upstream `Package.readPinInfo`.
    ///
    /// The two `yybegin(NAME)` calls are the whole reason KiCad's `(pin Round[A]Pad_600_um 1 0 0)`
    /// works: both the padstack reference and the pin name are integers in `YYINITIAL` and would
    /// be lost. A `(rotate …)` may appear either before the pin name or after the coordinates.
    fn read_pin_info(&mut self) -> Option<DsnPin> {
        self.lexer.begin(LexState::Name);
        let token = self.lexer.next_token();
        let Some(padstack_name) = token
            .as_ref()
            .and_then(Token::as_java_string)
            .map(str::to_string)
        else {
            self.warn("Package.read_pin_info: String or Integer expected");
            return None;
        };
        let mut rotation = 0.0f64;
        self.lexer.begin(LexState::Name);
        let mut token = self.lexer.next_token();
        if token == Some(Token::Open) {
            token = self.lexer.next_token();
            if token == Some(Token::Kw(Kw::Rotate)) {
                rotation = self.read_rotation();
            } else {
                self.skip_scope();
            }
            self.lexer.begin(LexState::Name);
            token = self.lexer.next_token();
        }
        let Some(pin_name) = token
            .as_ref()
            .and_then(Token::as_java_string)
            .map(str::to_string)
        else {
            self.warn("Package.read_pin_info: String or Integer expected");
            return None;
        };
        let mut rel_coor = [0.0f64; 2];
        for slot in rel_coor.iter_mut() {
            match self.lexer.next_token().as_ref().and_then(Token::as_number) {
                Some(value) => *slot = value,
                None => {
                    self.warn("Package.read_pin_info: number expected");
                    return None;
                }
            }
        }
        // Upstream enters this loop with `nextToken` still holding the last coordinate token, so
        // the first `prevToken` is that number and cannot open a scope. Consuming an extra token
        // here would eat the pin's closing bracket.
        let mut next: Option<Token> = None;
        loop {
            let prev = next;
            next = self.lexer.next_token();
            match &next {
                None => {
                    self.warn("Package.read_pin_info: unexpected end of file");
                    return None;
                }
                Some(Token::Close) => break,
                _ => {}
            }
            if prev == Some(Token::Open) {
                if next == Some(Token::Kw(Kw::Rotate)) {
                    rotation = self.read_rotation();
                } else {
                    self.skip_scope();
                }
            }
        }
        Some(DsnPin {
            padstack_name,
            pin_name,
            rel_coor,
            rotation,
        })
    }

    fn read_rotation(&mut self) -> f64 {
        let text = self.lexer.next_string_default();
        let result = text.parse::<f64>().unwrap_or(0.0);
        if !self.lexer.next_closing_bracket() {
            self.warn("Package.read_rotation: closing bracket expected");
        }
        result
    }

    // ---------------------------------------------------------------------------------------
    // network
    // ---------------------------------------------------------------------------------------

    fn read_network_scope(&mut self) -> bool {
        let mut next: Option<Token> = None;
        loop {
            let prev = next;
            next = self.lexer.next_token();
            match &next {
                None => {
                    self.warn("Network.read_scope: unexpected end of file");
                    return false;
                }
                Some(Token::Close) => return true,
                _ => {}
            }
            if prev != Some(Token::Open) {
                continue;
            }
            match &next {
                Some(Token::Kw(Kw::Net)) => {
                    if let Some(net) = self.read_net_scope() {
                        self.ast.nets.push(net);
                    }
                }
                Some(Token::Kw(Kw::Via)) => match self.read_via_info() {
                    Some(info) => self.ast.via_infos.push(info),
                    None => return false,
                },
                Some(Token::Kw(Kw::ViaRule)) => match self.read_via_rule() {
                    Some(rule) => self.ast.via_rules.push(rule),
                    None => return false,
                },
                Some(Token::Kw(Kw::Class)) => match self.read_net_class_scope() {
                    Some(class) => self.ast.net_classes.push(class),
                    None => return false,
                },
                Some(Token::Kw(Kw::ClassClass)) => match self.read_class_class_scope() {
                    Some(class_class) => self.ast.class_classes.push(class_class),
                    None => return false,
                },
                _ => self.skip_scope(),
            }
        }
    }

    /// Upstream `Network.readNetScope`.
    fn read_net_scope(&mut self) -> Option<DsnNet> {
        let name = self.lexer.next_string_default();
        let mut subnet_number = 1i32;
        let mut next = self.lexer.next_token();
        let scope_is_empty = next == Some(Token::Close);
        if let Some(value) = next.as_ref().and_then(Token::as_int) {
            subnet_number = value as i32;
        }
        let mut net = DsnNet {
            name,
            subnet_number,
            scope_is_empty,
            pins: Vec::new(),
            pin_order_found: false,
            fromto: Vec::new(),
            rules: Vec::new(),
        };
        if scope_is_empty {
            return Some(net);
        }
        loop {
            let prev = next;
            next = self.lexer.next_token();
            match &next {
                None => {
                    self.warn("Network.read_net_scope: unexpected end of file");
                    return None;
                }
                Some(Token::Close) => break,
                _ => {}
            }
            if prev != Some(Token::Open) {
                continue;
            }
            match &next {
                Some(Token::Kw(Kw::Pins)) => {
                    let mut pins = Vec::new();
                    self.read_net_pins(&mut pins);
                    net.pins.extend(pins);
                }
                Some(Token::Kw(Kw::Order)) => {
                    net.pin_order_found = true;
                    let mut pins = Vec::new();
                    self.read_net_pins(&mut pins);
                    net.pins.extend(pins);
                }
                Some(Token::Kw(Kw::Fromto)) => {
                    let mut pins = Vec::new();
                    self.read_net_pins(&mut pins);
                    // Upstream collects these into a TreeSet, so they are sorted and deduplicated.
                    pins.sort();
                    pins.dedup();
                    net.fromto.push(pins);
                }
                Some(Token::Kw(Kw::Rule)) => {
                    let rules = self.read_rule_scope();
                    net.rules.extend(rules);
                }
                Some(Token::Kw(Kw::LayerRule)) => {
                    self.warn("Network.read_net_scope: layer_rule not yet implemented");
                    self.skip_scope();
                }
                _ => self.skip_scope(),
            }
        }
        Some(net)
    }

    /// Upstream `Network.readNetPins`.
    ///
    /// `nextString(true, '-')` stops at the hyphen, `SPEC_CHAR` eats exactly that hyphen, and a
    /// second `nextString` takes the pin name. A component name may contain `-` only when it is
    /// quoted, because the quoted branch of the bypass reader has a different stop set.
    fn read_net_pins(&mut self, pins: &mut Vec<NetPin>) {
        loop {
            let component_name = self.lexer.next_string(true, b'-');
            if component_name.is_empty() {
                break;
            }
            self.lexer.begin(LexState::SpecChar);
            let _ = self.lexer.next_token();
            let pin_name = self.lexer.next_string(true, b' ');
            pins.push(NetPin {
                component_name,
                pin_name,
            });
        }
        if self.lexer.next_token() != Some(Token::Close) {
            self.warn("Network.read_net_pins: expected closed bracket is missing");
        }
    }

    /// Upstream `Network.readViaInfo`. Each of the three names is read with an explicit
    /// `yybegin(NAME)`, and the `.N` cleanup is deliberately **not** applied here.
    fn read_via_info(&mut self) -> Option<DsnViaInfo> {
        self.lexer.begin(LexState::Name);
        let name = self.read_name_token("Network.read_via_info")?;
        self.lexer.begin(LexState::Name);
        let padstack_name = self.read_name_token("Network.read_via_info")?;
        self.lexer.set_scope_identifier(&padstack_name);
        self.lexer.begin(LexState::Name);
        let clearance_class_name = self.read_name_token("Network.read_via_info")?;
        let mut attach_allowed = false;
        let token = self.lexer.next_token();
        if token != Some(Token::Close) {
            if token != Some(Token::Kw(Kw::Attach)) {
                self.warn("Network.read_via_info: attach expected");
                return None;
            }
            attach_allowed = true;
            if self.lexer.next_token() != Some(Token::Close) {
                self.warn("Network.read_via_info: closing bracket expected");
                return None;
            }
        }
        Some(DsnViaInfo {
            name,
            padstack_name,
            clearance_class_name,
            attach_allowed,
        })
    }

    fn read_name_token(&mut self, context: &str) -> Option<String> {
        let token = self.lexer.next_token();
        match token
            .as_ref()
            .filter(|t| t.is_string())
            .and_then(Token::as_java_string)
        {
            Some(text) => Some(text.to_string()),
            None => {
                self.warn(format!("{context}: string expected"));
                None
            }
        }
    }

    fn read_via_rule(&mut self) -> Option<Vec<String>> {
        let mut result = Vec::new();
        loop {
            self.lexer.begin(LexState::Name);
            let token = self.lexer.next_token();
            match &token {
                Some(Token::Close) | None => break,
                Some(value) if value.is_string() => {
                    result.push(value.as_java_string().unwrap_or_default().to_string());
                }
                _ => {
                    self.warn("Network.read_via_rule: string expected");
                    return None;
                }
            }
        }
        Some(result)
    }

    /// Upstream `NetClass.readScope`.
    fn read_net_class_scope(&mut self) -> Option<DsnNetClass> {
        self.lexer.begin(LexState::Name);
        let name = self.lexer.next_string_default();
        let net_list = self.lexer.next_string_list(b' ');
        let mut class = DsnNetClass {
            name,
            net_list,
            pull_tight: true,
            ..DsnNetClass::default()
        };
        let mut prev = self.lexer.next_token();
        loop {
            let next = self.lexer.next_token();
            match &next {
                None => {
                    self.warn("NetClass.read_scope: unexpected end of file");
                    return None;
                }
                Some(Token::Close) => break,
                _ => {}
            }
            if prev == Some(Token::Open) {
                match &next {
                    Some(Token::Kw(Kw::Rule)) => {
                        let rules = self.read_rule_scope();
                        class.rules.extend(rules);
                    }
                    Some(Token::Kw(Kw::LayerRule)) => {
                        if let Some(layer_rule) = self.read_layer_rule_scope() {
                            class.layer_rules.push(layer_rule);
                        }
                    }
                    Some(Token::Kw(Kw::ViaRule)) => class.via_rule = self.read_string_scope(),
                    Some(Token::Kw(Kw::Circuit)) => self.read_circuit_scope(&mut class),
                    Some(Token::Kw(Kw::ClearanceClass)) => {
                        class.trace_clearance_class = self.read_string_scope();
                        class.trace_clearance_class.as_ref()?;
                    }
                    Some(Token::Kw(Kw::ShoveFixed)) => class.shove_fixed = self.read_on_off_scope(),
                    Some(Token::Kw(Kw::PullTight)) => class.pull_tight = self.read_on_off_scope(),
                    _ => self.skip_scope(),
                }
            }
            prev = next;
        }
        Some(class)
    }

    /// Upstream `Circuit.readScope`.
    fn read_circuit_scope(&mut self, class: &mut DsnNetClass) {
        let mut next: Option<Token> = None;
        loop {
            let prev = next;
            next = self.lexer.next_token();
            match &next {
                None | Some(Token::Close) => return,
                _ => {}
            }
            if prev != Some(Token::Open) {
                continue;
            }
            match &next {
                Some(Token::Kw(Kw::Length)) => {
                    if let Some((max, min)) = self.read_length_scope() {
                        class.max_trace_length = max;
                        class.min_trace_length = min;
                    }
                }
                Some(Token::Kw(Kw::UseVia)) => {
                    let names = self.read_via_padstacks();
                    class.use_via.extend(names);
                }
                Some(Token::Kw(Kw::UseLayer)) => {
                    let names = self.read_string_list_scope();
                    class.use_layer.extend(names);
                }
                _ => self.skip_scope(),
            }
        }
    }

    fn read_length_scope(&mut self) -> Option<(f64, f64)> {
        let mut values = [0.0f64; 2];
        for slot in values.iter_mut() {
            match self.lexer.next_token().as_ref().and_then(Token::as_number) {
                Some(value) => *slot = value,
                None => {
                    self.warn("Circuit.read_length_scope: number expected");
                    return None;
                }
            }
        }
        let mut next: Option<Token> = None;
        loop {
            let prev = next;
            next = self.lexer.next_token();
            match &next {
                None | Some(Token::Close) => break,
                _ => {}
            }
            if prev == Some(Token::Open) {
                self.skip_scope();
            }
        }
        Some((values[0], values[1]))
    }

    fn read_class_class_scope(&mut self) -> Option<DsnClassClass> {
        let mut result = DsnClassClass::default();
        let mut prev: Option<Token> = None;
        loop {
            let next = self.lexer.next_token();
            match &next {
                None => {
                    self.warn("ClassClass.read_scope: unexpected end of file");
                    return None;
                }
                Some(Token::Close) => break,
                _ => {}
            }
            if prev == Some(Token::Open) {
                match &next {
                    Some(Token::Kw(Kw::Classes)) => {
                        let names = self.read_string_list_scope();
                        result.class_names.extend(names);
                    }
                    Some(Token::Kw(Kw::Rule)) => {
                        let rules = self.read_rule_scope();
                        result.rules.extend(rules);
                    }
                    Some(Token::Kw(Kw::LayerRule)) => {
                        if let Some(layer_rule) = self.read_layer_rule_scope() {
                            result.layer_rules.push(layer_rule);
                        }
                    }
                    _ => {}
                }
            }
            prev = next;
        }
        Some(result)
    }

    // ---------------------------------------------------------------------------------------
    // wiring
    // ---------------------------------------------------------------------------------------

    fn read_wiring_scope(&mut self) -> bool {
        self.ast.has_wiring_scope = true;
        let mut next: Option<Token> = None;
        loop {
            let prev = next;
            next = self.lexer.next_token();
            match &next {
                None => {
                    self.warn("Wiring.read_scope: unexpected end of file");
                    return false;
                }
                Some(Token::Close) => return true,
                _ => {}
            }
            if prev != Some(Token::Open) {
                continue;
            }
            match &next {
                Some(Token::Kw(Kw::Wire)) => {
                    if let Some(wire) = self.read_wire_scope() {
                        self.ast.wires.push(wire);
                    }
                }
                Some(Token::Kw(Kw::Via)) => match self.read_wiring_via_scope() {
                    Some(via) => self.ast.wiring_vias.push(via),
                    // A missing padstack aborts the *entire* wiring scope upstream.
                    None => return false,
                },
                _ => self.skip_scope(),
            }
        }
    }

    fn read_wire_scope(&mut self) -> Option<DsnWire> {
        let mut wire = DsnWire {
            path: None,
            border_shape: None,
            windows: Vec::new(),
            net: None,
            clearance_class_name: None,
            fixed: DsnFixedState::Unfixed,
        };
        let mut next: Option<Token> = None;
        loop {
            let prev = next;
            next = self.lexer.next_token();
            match &next {
                None => {
                    self.warn("Wiring.read_wire_scope: unexpected end of file");
                    return None;
                }
                Some(Token::Close) => break,
                _ => {}
            }
            if prev != Some(Token::Open) {
                continue;
            }
            match &next {
                Some(Token::Kw(Kw::PolygonPath)) => wire.path = self.read_polygon_path_scope(true),
                Some(Token::Kw(Kw::PolylinePath)) => {
                    wire.path = self.read_polyline_path_scope(true)
                }
                Some(Token::Kw(Kw::Rectangle)) => {
                    wire.border_shape = self.read_rectangle_scope(true)
                }
                Some(Token::Kw(Kw::Polygon)) => wire.border_shape = self.read_polygon_scope(true),
                Some(Token::Kw(Kw::Circle)) => wire.border_shape = self.read_circle_scope(true),
                Some(Token::Kw(Kw::Window)) => {
                    if let Some(hole) = self.read_shape_scope(true) {
                        wire.windows.push(hole);
                    }
                    if !self.lexer.next_closing_bracket() {
                        self.warn("Wiring.read_wire_scope: closing bracket expected");
                        return None;
                    }
                    next = Some(Token::Close);
                }
                Some(Token::Kw(Kw::Net)) => wire.net = self.read_net_id(),
                Some(Token::Kw(Kw::ClearanceClass)) => {
                    wire.clearance_class_name = self.read_string_scope()
                }
                Some(Token::Kw(Kw::Type)) => wire.fixed = self.calc_fixed(),
                _ => self.skip_scope(),
            }
        }
        if wire.path.is_none() && wire.border_shape.is_none() {
            self.warn("Wiring: wire has no shape");
            return None;
        }
        Some(wire)
    }

    /// Upstream `Wiring.calcFixed`.
    ///
    /// Only `shove_fixed`, `fix` and `normal` are recognized; everything else — `protect`, and
    /// crucially KiCad's `(type route)` — becomes `USER_FIXED`.
    fn calc_fixed(&mut self) -> DsnFixedState {
        let token = self.lexer.next_token();
        let result = match &token {
            Some(Token::Kw(Kw::ShoveFixed)) => DsnFixedState::ShoveFixed,
            Some(Token::Kw(Kw::Fix)) => DsnFixedState::SystemFixed,
            Some(Token::Kw(Kw::Normal)) => DsnFixedState::Unfixed,
            _ => DsnFixedState::UserFixed,
        };
        if self.lexer.next_token() != Some(Token::Close) {
            self.warn("Wiring.is_fixed: ) expected");
            return DsnFixedState::Unfixed;
        }
        result
    }

    /// Upstream `Wiring.readNetId`. A missing subnet number stays `0`, which downstream means
    /// "all subnets of this name".
    fn read_net_id(&mut self) -> Option<NetRef> {
        let name = self.lexer.next_string_default();
        self.lexer.set_scope_identifier(&name);
        let mut subnet_number = 0i32;
        let mut token = self.lexer.next_token();
        if let Some(value) = token.as_ref().and_then(Token::as_int) {
            subnet_number = value as i32;
            token = self.lexer.next_token();
        }
        if token != Some(Token::Close) {
            self.warn("Wiring.read_net_id: closing bracket expected");
        }
        Some(NetRef {
            name,
            subnet_number,
        })
    }

    fn read_wiring_via_scope(&mut self) -> Option<DsnWiringVia> {
        let token = self.lexer.next_token();
        let Some(padstack_name) = token
            .as_ref()
            .filter(|t| t.is_string())
            .and_then(Token::as_java_string)
            .map(str::to_string)
        else {
            self.warn("Wiring.read_via_scope: padstack name expected");
            return None;
        };
        self.lexer.set_scope_identifier(&padstack_name);
        let mut location = [0.0f64; 2];
        for slot in location.iter_mut() {
            match self.lexer.next_token().as_ref().and_then(Token::as_number) {
                Some(value) => *slot = value,
                None => {
                    self.warn("Wiring.read_via_scope: number expected");
                    return None;
                }
            }
        }
        let mut via = DsnWiringVia {
            padstack_name,
            location,
            net: None,
            clearance_class_name: None,
            fixed: DsnFixedState::Unfixed,
        };
        let mut next: Option<Token> = None;
        loop {
            let prev = next;
            next = self.lexer.next_token();
            match &next {
                None => {
                    self.warn("Wiring.read_via_scope: unexpected end of file");
                    return None;
                }
                Some(Token::Close) => break,
                _ => {}
            }
            if prev != Some(Token::Open) {
                continue;
            }
            match &next {
                Some(Token::Kw(Kw::Net)) => via.net = self.read_net_id(),
                Some(Token::Kw(Kw::ClearanceClass)) => {
                    via.clearance_class_name = self.read_string_scope()
                }
                Some(Token::Kw(Kw::Type)) => via.fixed = self.calc_fixed(),
                _ => self.skip_scope(),
            }
        }
        Some(via)
    }
}

/// The keywords that are `ScopeKeyword` instances upstream, i.e. the ones the generic driver
/// recurses into instead of skipping (`Keyword.java:26-118`).
fn is_scope_keyword(kw: Kw) -> bool {
    matches!(
        kw,
        Kw::ComponentScope
            | Kw::LibraryScope
            | Kw::NetworkScope
            | Kw::PartLibraryScope
            | Kw::ParserScope
            | Kw::PcbScope
            | Kw::PlaceControl
            | Kw::PlacementScope
            | Kw::PlaneScope
            | Kw::ResolutionScope
            | Kw::StructureScope
            | Kw::WiringScope
    )
}
