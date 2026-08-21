//! Recursive-descent parser for Specctra DSN files.

use crate::keyword::Keyword;
use crate::lexer::{DsnLexer, Token};
use crate::parser::*;

/// Parser for Specctra DSN files.
pub struct DsnReader<'a> {
    lexer: DsnLexer<'a>,
    current: Option<Token>,
}

impl<'a> DsnReader<'a> {
    pub fn new(input: &'a str) -> Self {
        let mut lexer = DsnLexer::new(input);
        let current = lexer.next_token();
        DsnReader { lexer, current }
    }

    fn advance(&mut self) -> Option<Token> {
        let prev = self.current.take();
        self.current = self.lexer.next_token();
        prev
    }

    fn peek(&self) -> Option<&Token> {
        self.current.as_ref()
    }

    fn eat_open_paren(&mut self) -> bool {
        if let Some(Token::OpenParen) = self.peek() {
            self.advance();
            true
        } else {
            false
        }
    }

    fn eat_close_paren(&mut self) -> bool {
        if let Some(Token::CloseParen) = self.peek() {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Skips the entire current scope (including nested parentheses).
    fn skip_scope(&mut self) {
        let mut depth = 1;
        while depth > 0 {
            match self.advance() {
                Some(Token::OpenParen) => depth += 1,
                Some(Token::CloseParen) => depth -= 1,
                None => break,
                _ => {}
            }
        }
    }

    /// Parses the top-level `(pcb ...)` scope.
    pub fn parse(&mut self) -> Result<DsnDocument, String> {
        if !self.eat_open_paren() {
            return Err("Expected '(' at start of DSN file".to_string());
        }

        match self.advance() {
            Some(Token::Keyword(Keyword::Pcb)) => {}
            Some(Token::String(ref s)) if s.eq_ignore_ascii_case("pcb") => {}
            _ => return Err("Expected 'pcb' keyword at start of file".to_string()),
        }

        let pcb_name = match self.advance() {
            Some(Token::String(s)) => s,
            Some(Token::Number(n)) => n.to_string(),
            _ => "unnamed_pcb".to_string(),
        };

        let mut doc = DsnDocument::new(&pcb_name);

        while let Some(tok) = self.peek() {
            if tok == &Token::CloseParen {
                self.advance();
                break;
            }
            if !self.eat_open_paren() {
                self.advance();
                continue;
            }

            match self.peek() {
                Some(Token::Keyword(Keyword::Parser)) => {
                    self.advance();
                    self.parse_parser_scope(&mut doc);
                }
                Some(Token::Keyword(Keyword::Resolution)) => {
                    self.advance();
                    self.parse_resolution(&mut doc);
                }
                Some(Token::Keyword(Keyword::Unit)) => {
                    self.advance();
                    self.parse_unit(&mut doc);
                }
                Some(Token::Keyword(Keyword::Structure)) => {
                    self.advance();
                    self.parse_structure_scope(&mut doc);
                }
                Some(Token::Keyword(Keyword::Placement)) => {
                    self.advance();
                    self.parse_placement_scope(&mut doc);
                }
                Some(Token::Keyword(Keyword::Library)) => {
                    self.advance();
                    self.parse_library_scope(&mut doc);
                }
                Some(Token::Keyword(Keyword::Network)) => {
                    self.advance();
                    self.parse_network_scope(&mut doc);
                }
                Some(Token::Keyword(Keyword::Wiring)) => {
                    self.advance();
                    self.parse_wiring_scope(&mut doc);
                }
                _ => {
                    self.advance();
                    self.skip_scope();
                }
            }
        }

        Ok(doc)
    }

    fn parse_parser_scope(&mut self, doc: &mut DsnDocument) {
        while let Some(tok) = self.peek() {
            if tok == &Token::CloseParen {
                self.advance();
                break;
            }
            if self.eat_open_paren() {
                match self.peek() {
                    Some(Token::Keyword(Keyword::Unit)) => {
                        self.advance();
                        self.parse_unit(doc);
                    }
                    Some(Token::Keyword(Keyword::Resolution)) => {
                        self.advance();
                        self.parse_resolution(doc);
                    }
                    _ => {
                        self.advance();
                        self.skip_scope();
                    }
                }
            } else {
                self.advance();
            }
        }
    }

    fn parse_unit(&mut self, doc: &mut DsnDocument) {
        if let Some(Token::String(s)) = self.advance() {
            doc.unit = s;
        }
        self.eat_close_paren();
    }

    fn parse_resolution(&mut self, doc: &mut DsnDocument) {
        // (resolution <unit> <val>) or (resolution <val>)
        while let Some(tok) = self.advance() {
            match tok {
                Token::Number(n) => doc.resolution = n,
                Token::CloseParen => break,
                _ => {}
            }
        }
    }

    fn parse_structure_scope(&mut self, doc: &mut DsnDocument) {
        while let Some(tok) = self.peek() {
            if tok == &Token::CloseParen {
                self.advance();
                break;
            }
            if self.eat_open_paren() {
                match self.peek() {
                    Some(Token::Keyword(Keyword::Layer)) => {
                        self.advance();
                        if let Some(Token::String(name)) = self.advance() {
                            doc.layers.push(DsnLayer {
                                name,
                                layer_type: "signal".to_string(),
                                preferred_direction: None,
                            });
                        }
                        self.skip_scope();
                    }
                    Some(Token::Keyword(Keyword::Boundary)) => {
                        self.advance();
                        self.parse_boundary(doc);
                    }
                    _ => {
                        self.advance();
                        self.skip_scope();
                    }
                }
            } else {
                self.advance();
            }
        }
    }

    fn parse_boundary(&mut self, doc: &mut DsnDocument) {
        // (boundary (path ...)) or (boundary (polygon ...))
        while let Some(tok) = self.peek() {
            if tok == &Token::CloseParen {
                self.advance();
                break;
            }
            if self.eat_open_paren() {
                match self.advance() {
                    Some(Token::Keyword(Keyword::Path)) | Some(Token::Keyword(Keyword::Polygon)) => {
                        // Skip layer name & width
                        self.advance(); // layer
                        self.advance(); // width (for path)
                        let mut coords = Vec::new();
                        while let Some(tok) = self.peek() {
                            if let Token::Number(n) = tok {
                                coords.push(*n);
                                self.advance();
                            } else {
                                break;
                            }
                        }
                        for chunk in coords.chunks_exact(2) {
                            doc.boundary_points.push(DsnPoint {
                                x: chunk[0],
                                y: chunk[1],
                            });
                        }
                        self.eat_close_paren();
                    }
                    _ => self.skip_scope(),
                }
            } else {
                self.advance();
            }
        }
    }

    fn parse_placement_scope(&mut self, doc: &mut DsnDocument) {
        while let Some(tok) = self.peek() {
            if tok == &Token::CloseParen {
                self.advance();
                break;
            }
            if self.eat_open_paren() {
                if let Some(Token::Keyword(Keyword::Component)) = self.peek() {
                    self.advance();
                    let pkg_name = match self.advance() {
                        Some(Token::String(s)) => s,
                        _ => "unknown".to_string(),
                    };
                    while let Some(tok) = self.peek() {
                        if tok == &Token::CloseParen {
                            self.advance();
                            break;
                        }
                        if self.eat_open_paren() {
                            if let Some(Token::Keyword(Keyword::Place)) = self.peek() {
                                self.advance();
                                let comp_name = match self.advance() {
                                    Some(Token::String(s)) => s,
                                    Some(Token::Number(n)) => n.to_string(),
                                    _ => "comp".to_string(),
                                };
                                let x = match self.advance() {
                                    Some(Token::Number(n)) => n,
                                    _ => 0.0,
                                };
                                let y = match self.advance() {
                                    Some(Token::Number(n)) => n,
                                    _ => 0.0,
                                };
                                let side = match self.advance() {
                                    Some(Token::String(s)) => s,
                                    _ => "front".to_string(),
                                };
                                let rot = match self.advance() {
                                    Some(Token::Number(n)) => n,
                                    _ => 0.0,
                                };
                                while let Some(tok) = self.peek() {
                                    if tok == &Token::CloseParen {
                                        self.advance();
                                        break;
                                    }
                                    if self.eat_open_paren() {
                                        self.skip_scope();
                                    } else {
                                        self.advance();
                                    }
                                }
                                doc.components.push(DsnComponent {
                                    name: comp_name,
                                    package_name: pkg_name.clone(),
                                    x,
                                    y,
                                    side,
                                    rotation: rot,
                                });
                            } else {
                                self.advance();
                                self.skip_scope();
                            }
                        } else {
                            self.advance();
                        }
                    }
                } else {
                    self.advance();
                    self.skip_scope();
                }
            } else {
                self.advance();
            }
        }
    }

    fn parse_library_scope(&mut self, doc: &mut DsnDocument) {
        while let Some(tok) = self.peek() {
            if tok == &Token::CloseParen {
                self.advance();
                break;
            }
            if self.eat_open_paren() {
                if let Some(Token::Keyword(Keyword::Image)) = self.peek() {
                    self.advance();
                    let pkg_name = match self.advance() {
                        Some(Token::String(s)) => s,
                        Some(Token::Number(n)) => n.to_string(),
                        _ => "unnamed_pkg".to_string(),
                    };
                    let mut pins = Vec::new();
                    while let Some(tok) = self.peek() {
                        if tok == &Token::CloseParen {
                            self.advance();
                            break;
                        }
                        if self.eat_open_paren() {
                            if let Some(Token::Keyword(Keyword::Pin)) = self.peek() {
                                self.advance();
                                let padstack_name = match self.advance() {
                                    Some(Token::String(s)) => s,
                                    Some(Token::Number(n)) => n.to_string(),
                                    _ => "default_padstack".to_string(),
                                };
                                while self.eat_open_paren() {
                                    self.skip_scope();
                                }
                                let pin_id = match self.advance() {
                                    Some(Token::String(s)) => s,
                                    Some(Token::Number(n)) => n.to_string(),
                                    _ => "1".to_string(),
                                };
                                let x = match self.advance() {
                                    Some(Token::Number(n)) => n,
                                    _ => 0.0,
                                };
                                let y = match self.advance() {
                                    Some(Token::Number(n)) => n,
                                    _ => 0.0,
                                };
                                while let Some(tok) = self.peek() {
                                    if tok == &Token::CloseParen {
                                        self.advance();
                                        break;
                                    }
                                    if self.eat_open_paren() {
                                        self.skip_scope();
                                    } else {
                                        self.advance();
                                    }
                                }
                                pins.push(DsnPin {
                                    pin_id,
                                    padstack_name,
                                    x,
                                    y,
                                    rotation: 0.0,
                                });
                            } else {
                                self.advance();
                                self.skip_scope();
                            }
                        } else {
                            self.advance();
                        }
                    }
                    doc.packages.push(DsnPackage {
                        name: pkg_name,
                        pins,
                        outlines: Vec::new(),
                    });
                } else if let Some(Token::Keyword(Keyword::Padstack)) = self.peek() {
                    self.advance();
                    let padstack_name = match self.advance() {
                        Some(Token::String(s)) => s,
                        Some(Token::Number(n)) => n.to_string(),
                        _ => "padstack".to_string(),
                    };
                    let mut shapes = Vec::new();
                    while let Some(tok) = self.peek() {
                        if tok == &Token::CloseParen {
                            self.advance();
                            break;
                        }
                        if self.eat_open_paren() {
                            if let Some(Token::Keyword(Keyword::Shape)) = self.peek() {
                                self.advance();
                                if self.eat_open_paren() {
                                    let shape_type = match self.advance() {
                                        Some(Token::Keyword(Keyword::Circle)) => "circle".to_string(),
                                        Some(Token::Keyword(Keyword::Rectangle)) => "rect".to_string(),
                                        Some(Token::Keyword(Keyword::Polygon)) => "polygon".to_string(),
                                        Some(Token::String(s)) => s,
                                        _ => "circle".to_string(),
                                    };
                                    let layer = match self.advance() {
                                        Some(Token::String(s)) => s,
                                        Some(Token::Number(n)) => n.to_string(),
                                        _ => "F.Cu".to_string(),
                                    };
                                    let mut dims = Vec::new();
                                    while let Some(tok) = self.peek() {
                                        if tok == &Token::CloseParen {
                                            self.advance();
                                            break;
                                        }
                                        match self.advance() {
                                            Some(Token::Number(n)) => dims.push(n),
                                            _ => {}
                                        }
                                    }
                                    shapes.push(DsnPadstackShape {
                                        layer,
                                        shape_type,
                                        dimensions: dims,
                                        points: Vec::new(),
                                    });
                                }
                                while let Some(tok) = self.peek() {
                                    if tok == &Token::CloseParen {
                                        self.advance();
                                        break;
                                    }
                                    if self.eat_open_paren() {
                                        self.skip_scope();
                                    } else {
                                        self.advance();
                                    }
                                }
                            } else {
                                self.advance();
                                self.skip_scope();
                            }
                        } else {
                            self.advance();
                        }
                    }
                    doc.padstacks.push(DsnPadstack {
                        name: padstack_name,
                        shapes,
                    });
                } else {
                    self.advance();
                    self.skip_scope();
                }
            } else {
                self.advance();
            }
        }
    }

    fn parse_network_scope(&mut self, doc: &mut DsnDocument) {
        while let Some(tok) = self.peek() {
            if tok == &Token::CloseParen {
                self.advance();
                break;
            }
            if self.eat_open_paren() {
                match self.peek() {
                    Some(Token::Keyword(Keyword::Net)) => {
                        self.advance();
                        let net_name = match self.advance() {
                            Some(Token::String(s)) => s,
                            Some(Token::Number(n)) => n.to_string(),
                            _ => "unnamed_net".to_string(),
                        };
                        let mut pins = Vec::new();
                        while let Some(tok) = self.peek() {
                            if tok == &Token::CloseParen {
                                self.advance();
                                break;
                            }
                            if self.eat_open_paren() {
                                if let Some(Token::Keyword(Keyword::Pins)) = self.peek() {
                                    self.advance();
                                    while let Some(tok) = self.advance() {
                                        match tok {
                                            Token::String(s) => {
                                                if let Some((comp, pin)) = s.rsplit_once('-') {
                                                    pins.push(DsnNetPin {
                                                        component_name: comp.to_string(),
                                                        pin_id: pin.to_string(),
                                                    });
                                                }
                                            }
                                            Token::Number(n) => {
                                                let s = n.to_string();
                                                if let Some((comp, pin)) = s.rsplit_once('-') {
                                                    pins.push(DsnNetPin {
                                                        component_name: comp.to_string(),
                                                        pin_id: pin.to_string(),
                                                    });
                                                }
                                            }
                                            Token::CloseParen => break,
                                            _ => {}
                                        }
                                    }
                                } else {
                                    self.advance();
                                    self.skip_scope();
                                }
                            } else {
                                self.advance();
                            }
                        }
                        doc.nets.push(DsnNet {
                            name: net_name,
                            pins,
                            class_name: None,
                            is_plane: false,
                        });
                    }
                    Some(Token::Keyword(Keyword::Class)) => {
                        self.advance();
                        let class_name = match self.advance() {
                            Some(Token::String(s)) => s,
                            Some(Token::Number(n)) => n.to_string(),
                            _ => "default".to_string(),
                        };
                        let mut net_names = Vec::new();
                        while let Some(tok) = self.advance() {
                            match tok {
                                Token::String(s) => net_names.push(s),
                                Token::Number(n) => net_names.push(n.to_string()),
                                Token::CloseParen => break,
                                _ => {}
                            }
                        }
                        doc.classes.push(DsnClass {
                            name: class_name,
                            net_names,
                            width: None,
                            clearance: None,
                            via_rule: None,
                        });
                    }
                    _ => {
                        self.advance();
                        self.skip_scope();
                    }
                }
            } else {
                self.advance();
            }
        }
    }

    fn parse_wiring_scope(&mut self, doc: &mut DsnDocument) {
        while let Some(tok) = self.peek() {
            if tok == &Token::CloseParen {
                self.advance();
                break;
            }
            if self.eat_open_paren() {
                match self.peek() {
                    Some(Token::Keyword(Keyword::Wire)) => {
                        self.advance();
                        let mut layer = "F.Cu".to_string();
                        let mut width = 250.0;
                        let mut points = Vec::new();
                        let mut net_name = "GND".to_string();
                        while let Some(tok) = self.peek() {
                            if tok == &Token::CloseParen {
                                self.advance();
                                break;
                            }
                            if self.eat_open_paren() {
                                if let Some(Token::Keyword(Keyword::Path)) = self.peek() {
                                    self.advance();
                                    layer = match self.advance() {
                                        Some(Token::String(s)) => s,
                                        Some(Token::Number(n)) => n.to_string(),
                                        _ => "F.Cu".to_string(),
                                    };
                                    width = match self.advance() {
                                        Some(Token::Number(n)) => n,
                                        _ => 250.0,
                                    };
                                    let mut coords = Vec::new();
                                    while let Some(tok) = self.peek() {
                                        if tok == &Token::CloseParen {
                                            self.advance();
                                            break;
                                        }
                                        match self.advance() {
                                            Some(Token::Number(n)) => coords.push(n),
                                            _ => {}
                                        }
                                    }
                                    for chunk in coords.chunks(2) {
                                        if chunk.len() == 2 {
                                            points.push(DsnPoint { x: chunk[0], y: chunk[1] });
                                        }
                                    }
                                } else if let Some(Token::Keyword(Keyword::Net)) = self.peek() {
                                    self.advance();
                                    net_name = match self.advance() {
                                        Some(Token::String(s)) => s,
                                        Some(Token::Number(n)) => n.to_string(),
                                        _ => "GND".to_string(),
                                    };
                                    self.eat_close_paren();
                                } else {
                                    self.advance();
                                    self.skip_scope();
                                }
                            } else {
                                self.advance();
                            }
                        }
                        doc.wires.push(DsnWire {
                            net_name,
                            layer,
                            width,
                            points,
                        });
                    }
                    Some(Token::Keyword(Keyword::Via)) => {
                        self.advance();
                        let padstack_name = match self.advance() {
                            Some(Token::String(s)) => s,
                            Some(Token::Number(n)) => n.to_string(),
                            _ => "via".to_string(),
                        };
                        let x = match self.advance() {
                            Some(Token::Number(n)) => n,
                            _ => 0.0,
                        };
                        let y = match self.advance() {
                            Some(Token::Number(n)) => n,
                            _ => 0.0,
                        };
                        let mut net_name = "GND".to_string();
                        while let Some(tok) = self.peek() {
                            if tok == &Token::CloseParen {
                                self.advance();
                                break;
                            }
                            if self.eat_open_paren() {
                                if let Some(Token::Keyword(Keyword::Net)) = self.peek() {
                                    self.advance();
                                    net_name = match self.advance() {
                                        Some(Token::String(s)) => s,
                                        Some(Token::Number(n)) => n.to_string(),
                                        _ => "GND".to_string(),
                                    };
                                    self.eat_close_paren();
                                } else {
                                    self.advance();
                                    self.skip_scope();
                                }
                            } else {
                                self.advance();
                            }
                        }
                        doc.vias.push(DsnVia {
                            net_name,
                            padstack_name,
                            x,
                            y,
                        });
                    }
                    _ => {
                        self.advance();
                        self.skip_scope();
                    }
                }
            } else {
                self.advance();
            }
        }
    }
}

/// Helper function to parse a DSN string into a `DsnDocument`.
pub fn parse_dsn(input: &str) -> Result<DsnDocument, String> {
    let mut reader = DsnReader::new(input);
    reader.parse()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sample_dsn() {
        let sample = r#"
        (pcb "tutorial_board"
            (parser
                (unit mm)
                (resolution mm 1000)
            )
            (structure
                (layer F.Cu (type signal))
                (layer B.Cu (type signal))
                (boundary
                    (path pcb 0 0 0 100 0 100 100 0 100 0 0)
                )
            )
            (placement
                (component "DIP8"
                    (place "U1" 10.0 20.0 front 0.0)
                )
            )
            (network
                (net "GND"
                    (pins "U1-4" "U1-8")
                )
                (class "power" "GND")
            )
        )
        "#;

        let doc = parse_dsn(sample).unwrap();
        assert_eq!(doc.pcb_name, "tutorial_board");
        assert_eq!(doc.unit, "mm");
        assert_eq!(doc.resolution, 1000.0);
        assert_eq!(doc.layers.len(), 2);
        assert_eq!(doc.boundary_points.len(), 5);
        assert_eq!(doc.components.len(), 1);
        assert_eq!(doc.components[0].name, "U1");
        assert_eq!(doc.components[0].package_name, "DIP8");
        assert_eq!(doc.nets.len(), 1);
        assert_eq!(doc.nets[0].name, "GND");
        assert_eq!(doc.nets[0].pins.len(), 2);
        assert_eq!(doc.classes.len(), 1);
    }
}
