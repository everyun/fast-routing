//! Specctra DSN S-expression AST tree parser.
//!
//! Parses complete DSN documents into a hierarchical S-expression tree first,
//! then extracts layers, boundaries, placement, padstacks, networks, and wiring
//! with zero parenthesis de-synchronization risk.

use crate::lexer::{DsnLexer, Token};
use crate::parser::*;

/// S-expression node in a Specctra DSN or SES file.
#[derive(Debug, Clone, PartialEq)]
pub enum SExpr {
    Atom(String),
    Number(f64),
    List(Vec<SExpr>),
}

impl SExpr {
    /// Recursively parses an S-expression from the token slice.
    pub fn parse_from_tokens(tokens: &[Token]) -> Option<SExpr> {
        let mut cursor = 0;
        Self::parse_rec(tokens, &mut cursor)
    }

    fn parse_rec(tokens: &[Token], cursor: &mut usize) -> Option<SExpr> {
        if *cursor >= tokens.len() {
            return None;
        }
        let tok = &tokens[*cursor];
        *cursor += 1;
        match tok {
            Token::OpenParen => {
                let mut items = Vec::new();
                while *cursor < tokens.len() {
                    if tokens[*cursor] == Token::CloseParen {
                        *cursor += 1;
                        break;
                    }
                    if let Some(child) = Self::parse_rec(tokens, cursor) {
                        items.push(child);
                    } else {
                        break;
                    }
                }
                Some(SExpr::List(items))
            }
            Token::CloseParen => None,
            Token::Keyword(kw) => Some(SExpr::Atom(format!("{:?}", kw).to_ascii_lowercase())),
            Token::String(s) => Some(SExpr::Atom(s.clone())),
            Token::Number(n) => Some(SExpr::Number(*n)),
        }
    }

    pub fn atom(&self) -> Option<&str> {
        match self {
            SExpr::Atom(s) => Some(s),
            _ => None,
        }
    }

    pub fn number(&self) -> Option<f64> {
        match self {
            SExpr::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn list(&self) -> Option<&[SExpr]> {
        match self {
            SExpr::List(l) => Some(l),
            _ => None,
        }
    }

    pub fn head_matches(&self, keyword: &str) -> bool {
        if let Some(list) = self.list() {
            if let Some(first) = list.first() {
                if let Some(name) = first.atom() {
                    return name.eq_ignore_ascii_case(keyword);
                }
            }
        }
        false
    }
}

/// Parses a DSN string into a strongly-typed `DsnDocument`.
pub struct DsnReader;

impl DsnReader {
    pub fn parse(input: &str) -> Result<DsnDocument, String> {
        let mut lexer = DsnLexer::new(input);
        let tokens = lexer.tokenize_all();
        let root = SExpr::parse_from_tokens(&tokens)
            .ok_or_else(|| "Failed to parse root S-expression in DSN".to_string())?;

        let items = root
            .list()
            .ok_or_else(|| "Root S-expression must be a list (pcb ...)".to_string())?;

        if items.is_empty() {
            return Err("Empty DSN document".to_string());
        }

        let raw_name = items
            .get(1)
            .and_then(|e| e.atom())
            .unwrap_or("unnamed_pcb");
        let pcb_name = if raw_name.trim().is_empty() {
            "unnamed_pcb".to_string()
        } else {
            raw_name.to_string()
        };

        let mut doc = DsnDocument::new(&pcb_name);

        for item in items.iter().skip(1) {
            if let Some(list) = item.list() {
                let scope_name = list
                    .first()
                    .and_then(|e| e.atom())
                    .unwrap_or("")
                    .to_ascii_lowercase();

                match scope_name.as_str() {
                    "parser" => Self::extract_parser(list, &mut doc),
                    "resolution" => Self::extract_resolution(list, &mut doc),
                    "unit" => Self::extract_unit(list, &mut doc),
                    "structure" => Self::extract_structure(list, &mut doc),
                    "placement" => Self::extract_placement(list, &mut doc),
                    "library" => Self::extract_library(list, &mut doc),
                    "network" => Self::extract_network(list, &mut doc),
                    "wiring" => Self::extract_wiring(list, &mut doc),
                    _ => {}
                }
            }
        }

        Ok(doc)
    }

    fn extract_parser(list: &[SExpr], doc: &mut DsnDocument) {
        for item in list.iter().skip(1) {
            if let Some(sub) = item.list() {
                if sub.first().and_then(|e| e.atom()).map(|s| s.eq_ignore_ascii_case("unit")).unwrap_or(false) {
                    if let Some(unit_str) = sub.get(1).and_then(|e| e.atom()) {
                        doc.unit = unit_str.to_string();
                    }
                } else if sub.first().and_then(|e| e.atom()).map(|s| s.eq_ignore_ascii_case("resolution")).unwrap_or(false) {
                    if let Some(unit_str) = sub.get(1).and_then(|e| e.atom()) {
                        doc.unit = unit_str.to_string();
                    }
                    if let Some(res_num) = sub.get(2).and_then(|e| e.number()) {
                        doc.resolution = res_num;
                    }
                }
            }
        }
    }

    fn extract_resolution(list: &[SExpr], doc: &mut DsnDocument) {
        if let Some(unit_str) = list.get(1).and_then(|e| e.atom()) {
            doc.unit = unit_str.to_string();
        }
        if let Some(res_num) = list.get(2).and_then(|e| e.number()) {
            doc.resolution = res_num;
        }
    }

    fn extract_unit(list: &[SExpr], doc: &mut DsnDocument) {
        if let Some(unit_str) = list.get(1).and_then(|e| e.atom()) {
            doc.unit = unit_str.to_string();
        }
    }

    fn extract_structure(list: &[SExpr], doc: &mut DsnDocument) {
        for item in list.iter().skip(1) {
            if let Some(sub) = item.list() {
                let tag = sub.first().and_then(|e| e.atom()).unwrap_or("");
                if tag.eq_ignore_ascii_case("layer") {
                    let layer_name = sub.get(1).and_then(|e| e.atom()).unwrap_or("unnamed_layer").to_string();
                    let mut layer_type = "signal".to_string();
                    for child in sub.iter().skip(2) {
                        if let Some(c_list) = child.list() {
                            if c_list.first().and_then(|e| e.atom()).map(|s| s.eq_ignore_ascii_case("type")).unwrap_or(false) {
                                if let Some(t) = c_list.get(1).and_then(|e| e.atom()) {
                                    layer_type = t.to_string();
                                }
                            }
                        }
                    }
                    doc.layers.push(DsnLayer {
                        name: layer_name,
                        layer_type,
                        preferred_direction: None,
                    });
                } else if tag.eq_ignore_ascii_case("boundary") {
                    for child in sub.iter().skip(1) {
                        if let Some(c_list) = child.list() {
                            if c_list.first().and_then(|e| e.atom()).map(|s| s.eq_ignore_ascii_case("path")).unwrap_or(false) {
                                let mut nums = Vec::new();
                                for val in c_list.iter().skip(3) {
                                    if let Some(n) = val.number() {
                                        nums.push(n);
                                    }
                                }
                                for chunk in nums.chunks(2) {
                                    if chunk.len() == 2 {
                                        doc.boundary_points.push(DsnPoint { x: chunk[0], y: chunk[1] });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn extract_placement(list: &[SExpr], doc: &mut DsnDocument) {
        for item in list.iter().skip(1) {
            if let Some(sub) = item.list() {
                if sub.first().and_then(|e| e.atom()).map(|s| s.eq_ignore_ascii_case("component")).unwrap_or(false) {
                    let pkg_name = sub.get(1).and_then(|e| e.atom()).unwrap_or("unnamed_pkg").to_string();
                    for child in sub.iter().skip(2) {
                        if let Some(c_list) = child.list() {
                            if c_list.first().and_then(|e| e.atom()).map(|s| s.eq_ignore_ascii_case("place")).unwrap_or(false) {
                                let comp_name = c_list.get(1).and_then(|e| e.atom()).unwrap_or("unnamed_comp").to_string();
                                let x = c_list.get(2).and_then(|e| e.number()).unwrap_or(0.0);
                                let y = c_list.get(3).and_then(|e| e.number()).unwrap_or(0.0);
                                let side = c_list.get(4).and_then(|e| e.atom()).unwrap_or("front").to_string();
                                let rotation = c_list.get(5).and_then(|e| e.number()).unwrap_or(0.0);
                                doc.components.push(DsnComponent {
                                    name: comp_name,
                                    package_name: pkg_name.clone(),
                                    x,
                                    y,
                                    side,
                                    rotation,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    fn extract_library(list: &[SExpr], doc: &mut DsnDocument) {
        for item in list.iter().skip(1) {
            if let Some(sub) = item.list() {
                let tag = sub.first().and_then(|e| e.atom()).unwrap_or("");
                if tag.eq_ignore_ascii_case("image") {
                    let pkg_name = sub.get(1).and_then(|e| e.atom()).unwrap_or("unnamed_pkg").to_string();
                    let mut pins = Vec::new();
                    for child in sub.iter().skip(2) {
                        if let Some(c_list) = child.list() {
                            if c_list.first().and_then(|e| e.atom()).map(|s| s.eq_ignore_ascii_case("pin")).unwrap_or(false) {
                                let padstack_name = c_list.get(1).and_then(|e| e.atom()).unwrap_or("default").to_string();
                                // Find pin ID and (x, y) coordinates skipping any nested scopes like (rotate ...)
                                let mut pin_id = "1".to_string();
                                let mut coords = Vec::new();
                                for elem in c_list.iter().skip(2) {
                                    if let Some(id_str) = elem.atom() {
                                        pin_id = id_str.to_string();
                                    } else if let Some(n) = elem.number() {
                                        coords.push(n);
                                    }
                                }
                                let x = coords.first().copied().unwrap_or(0.0);
                                let y = coords.get(1).copied().unwrap_or(0.0);
                                pins.push(DsnPin {
                                    pin_id,
                                    padstack_name,
                                    x,
                                    y,
                                    rotation: 0.0,
                                });
                            }
                        }
                    }
                    doc.packages.push(DsnPackage {
                        name: pkg_name,
                        pins,
                        outlines: Vec::new(),
                    });
                } else if tag.eq_ignore_ascii_case("padstack") {
                    let padstack_name = sub.get(1).and_then(|e| e.atom()).unwrap_or("padstack").to_string();
                    let mut shapes = Vec::new();
                    for child in sub.iter().skip(2) {
                        if let Some(c_list) = child.list() {
                            if c_list.first().and_then(|e| e.atom()).map(|s| s.eq_ignore_ascii_case("shape")).unwrap_or(false) {
                                if let Some(shape_item) = c_list.get(1).and_then(|e| e.list()) {
                                    let shape_type = shape_item.first().and_then(|e| e.atom()).unwrap_or("circle").to_string();
                                    let layer = shape_item.get(1).and_then(|e| e.atom()).unwrap_or("F.Cu").to_string();
                                    let mut dims = Vec::new();
                                    for dim_val in shape_item.iter().skip(2) {
                                        if let Some(n) = dim_val.number() {
                                            dims.push(n);
                                        }
                                    }
                                    shapes.push(DsnPadstackShape {
                                        layer,
                                        shape_type,
                                        dimensions: dims,
                                        points: Vec::new(),
                                    });
                                }
                            }
                        }
                    }
                    doc.padstacks.push(DsnPadstack {
                        name: padstack_name,
                        shapes,
                    });
                }
            }
        }
    }

    fn extract_network(list: &[SExpr], doc: &mut DsnDocument) {
        for item in list.iter().skip(1) {
            if let Some(sub) = item.list() {
                let tag = sub.first().and_then(|e| e.atom()).unwrap_or("");
                if tag.eq_ignore_ascii_case("net") {
                    let net_name = sub.get(1).and_then(|e| e.atom()).unwrap_or("unnamed_net").to_string();
                    let mut pins = Vec::new();
                    for child in sub.iter().skip(2) {
                        if let Some(c_list) = child.list() {
                            if c_list.first().and_then(|e| e.atom()).map(|s| s.eq_ignore_ascii_case("pins")).unwrap_or(false) {
                                for pin_val in c_list.iter().skip(1) {
                                    if let Some(pin_str) = pin_val.atom() {
                                        if let Some((comp, pin)) = pin_str.rsplit_once('-') {
                                            pins.push(DsnNetPin {
                                                component_name: comp.to_string(),
                                                pin_id: pin.to_string(),
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }
                    doc.nets.push(DsnNet {
                        name: net_name,
                        pins,
                        class_name: None,
                        is_plane: false,
                    });
                } else if tag.eq_ignore_ascii_case("class") {
                    let class_name = sub.get(1).and_then(|e| e.atom()).unwrap_or("default").to_string();
                    let mut net_names = Vec::new();
                    let mut width = None;
                    let mut clearance = None;
                    let mut via_rule = None;

                    for elem in sub.iter().skip(2) {
                        if let Some(s) = elem.atom() {
                            net_names.push(s.to_string());
                        } else if let Some(c_list) = elem.list() {
                            let c_tag = c_list.first().and_then(|e| e.atom()).unwrap_or("");
                            if c_tag.eq_ignore_ascii_case("circuit") {
                                for sub_c in c_list.iter().skip(1) {
                                    if let Some(sc_list) = sub_c.list() {
                                        if let Some(via_name) = sc_list.get(1).and_then(|e| e.atom()) {
                                            via_rule = Some(via_name.to_string());
                                        }
                                    }
                                }
                            } else if c_tag.eq_ignore_ascii_case("rule") {
                                for sub_r in c_list.iter().skip(1) {
                                    if let Some(sr_list) = sub_r.list() {
                                        let r_tag = sr_list.first().and_then(|e| e.atom()).unwrap_or("");
                                        if r_tag.eq_ignore_ascii_case("width") {
                                            width = sr_list.get(1).and_then(|e| e.number());
                                        } else if r_tag.eq_ignore_ascii_case("clearance") {
                                            clearance = sr_list.get(1).and_then(|e| e.number());
                                        }
                                    }
                                }
                            }
                        }
                    }

                    doc.classes.push(DsnClass {
                        name: class_name,
                        net_names,
                        width,
                        clearance,
                        via_rule,
                    });
                }
            }
        }
    }

    fn extract_wiring(list: &[SExpr], doc: &mut DsnDocument) {
        for item in list.iter().skip(1) {
            if let Some(sub) = item.list() {
                let tag = sub.first().and_then(|e| e.atom()).unwrap_or("");
                if tag.eq_ignore_ascii_case("wire") {
                    let mut layer = "F.Cu".to_string();
                    let mut width = 250.0;
                    let mut points = Vec::new();
                    let mut net_name = "GND".to_string();
                    let mut fixed_type = None;

                    for child in sub.iter().skip(1) {
                        if let Some(c_list) = child.list() {
                            let c_tag = c_list.first().and_then(|e| e.atom()).unwrap_or("");
                            if c_tag.eq_ignore_ascii_case("path") {
                                if let Some(lyr) = c_list.get(1).and_then(|e| e.atom()) {
                                    layer = lyr.to_string();
                                }
                                if let Some(w) = c_list.get(2).and_then(|e| e.number()) {
                                    width = w;
                                }
                                let mut nums = Vec::new();
                                for val in c_list.iter().skip(3) {
                                    if let Some(n) = val.number() {
                                        nums.push(n);
                                    }
                                }
                                for chunk in nums.chunks(2) {
                                    if chunk.len() == 2 {
                                        points.push(DsnPoint { x: chunk[0], y: chunk[1] });
                                    }
                                }
                            } else if c_tag.eq_ignore_ascii_case("net") {
                                if let Some(n) = c_list.get(1).and_then(|e| e.atom()) {
                                    net_name = n.to_string();
                                }
                            } else if c_tag.eq_ignore_ascii_case("type") {
                                if let Some(t) = c_list.get(1).and_then(|e| e.atom()) {
                                    fixed_type = Some(t.to_string());
                                }
                            }
                        }
                    }

                    doc.wires.push(DsnWire {
                        net_name,
                        layer,
                        width,
                        points,
                        fixed_type,
                    });
                } else if tag.eq_ignore_ascii_case("via") {
                    let padstack_name = sub.get(1).and_then(|e| e.atom()).unwrap_or("via").to_string();
                    let x = sub.get(2).and_then(|e| e.number()).unwrap_or(0.0);
                    let y = sub.get(3).and_then(|e| e.number()).unwrap_or(0.0);
                    let mut net_name = "GND".to_string();
                    let mut fixed_type = None;

                    for child in sub.iter().skip(3) {
                        if let Some(c_list) = child.list() {
                            let c_tag = c_list.first().and_then(|e| e.atom()).unwrap_or("");
                            if c_tag.eq_ignore_ascii_case("net") {
                                if let Some(n) = c_list.get(1).and_then(|e| e.atom()) {
                                    net_name = n.to_string();
                                }
                            } else if c_tag.eq_ignore_ascii_case("type") {
                                if let Some(t) = c_list.get(1).and_then(|e| e.atom()) {
                                    fixed_type = Some(t.to_string());
                                }
                            }
                        }
                    }

                    doc.vias.push(DsnVia {
                        net_name,
                        padstack_name,
                        x,
                        y,
                        fixed_type,
                    });
                }
            }
        }
    }
}

/// Helper function to parse a DSN string into a `DsnDocument`.
pub fn parse_dsn(input: &str) -> Result<DsnDocument, String> {
    DsnReader::parse(input)
}
