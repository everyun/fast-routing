//! The Specctra DSN tokenizer.
//!
//! A byte-for-byte port of upstream's `io/specctra/parser/SpecctraDsnStreamReader.java`, which is
//! two scanners glued together:
//!
//! * a JFlex DFA (`SpecctraFileDescription.flex`) driven by an explicit, caller-settable lexical
//!   state — [`LexState`] here, `yybegin` there;
//! * [`Lexer::next_string`], a **raw-buffer bypass** that does not run the DFA at all and has its
//!   own quoting rules.
//!
//! Both are load-bearing; `dsn-board-semantics.md` §2 explains why. The three properties that a
//! "reasonable" tokenizer gets wrong and this one does not:
//!
//! 1. **Tokens keep their raw spelling.** A DSN pin called `007`, a layer called `12` and a net
//!    called `1` all arrive as text, because their consumers force [`LexState::Name`] first and
//!    then `toString()` whatever token they get. [`Token::Int`] and [`Token::Float`] therefore
//!    carry the source text alongside the parsed value, and [`Token::as_java_string`] hands the
//!    source text back.
//! 2. **Quoting is positional and the two paths disagree.** In the DFA path a leading `"` opens a
//!    string that runs to the next `"` *across newlines*; in the [`Lexer::next_string`] path the
//!    same string is terminated by `"` **or end of line**, and `\` is never an escape in either.
//!    That is what makes KiCad's unescaped inch marks inside `(PN "1/4\"")`-style strings
//!    survive instead of swallowing the rest of the file.
//! 3. **`(string_quote ')` does not change the bypass path.** Only ASCII `"` (34) opens a
//!    bypass-quoted string, whatever the directive said. `'`-quoting works only through
//!    [`Lexer::next_token`].
//!
//! The `%ignorecase` directive on the grammar means keywords match case-insensitively — real
//! files rely on it (`(PCB ...)` from Eagle, `front`/`Front` from `eagle2freerouting`).

/// The scanner's lexical state — upstream's `yybegin` argument.
///
/// Two mechanisms set it: the DFA itself, for keywords whose next token is a name (see
/// [`Kw::entered_state`]), and explicit calls from the scope readers ([`Lexer::begin`]). Producing
/// a token from any state other than [`LexState::Initial`] resets it to [`LexState::Initial`],
/// exactly like the generated scanner's per-rule `yybegin(YYINITIAL)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LexState {
    /// `YYINITIAL`: keywords are recognized and numbers lex as numbers.
    Initial,
    /// `NAME`: the next token is a name even if it starts with a digit, `-` or `+`.
    Name,
    /// `LAYER_NAME`: like [`LexState::Name`], except that `pcb` and `signal` still resolve to
    /// their keywords (`Shape.getLayer` compares against exactly those two).
    LayerName,
    /// `COMPONENT_NAME`: like [`LexState::Name`] but `-` is excluded from the identifier, so
    /// `U1-3` splits. Unused by the v2 readers, kept because the DFA has it.
    ComponentName,
    /// `SPEC_CHAR`: return exactly one special character — used to eat the `-` between a
    /// component name and a pin name in `(pins U1-3 ...)`.
    SpecChar,
    /// `IGNORE_QUOTE`: read an identifier treating `"` and `'` as ordinary characters. Exists
    /// solely so `(string_quote ")` can name the quote character.
    IgnoreQuote,
}

/// The keywords the DFA recognizes, with upstream's alias spellings folded in.
///
/// `Keyword.PN` and `Keyword.JUMPER` are deliberately absent: upstream declares them but the DFA
/// never produces them, so `(PN "…")` and `(type jumper)` arrive as *strings* and their readers
/// compare text. Reproducing that is the whole point (`dsn-board-semantics.md` §2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(missing_docs)]
pub enum Kw {
    Absolute,
    Active,
    AgainstPreferredDirectionTraceCosts,
    Attach,
    Autoroute,
    AutorouteSettings,
    Back,
    Boundary,
    Circle,
    Circuit,
    Class,
    ClassClass,
    Classes,
    Clearance,
    ClearanceClass,
    ComponentScope,
    Constant,
    Control,
    Fanout,
    Fix,
    FortyfiveDegree,
    FlipStyle,
    Fromto,
    Front,
    GeneratedByFreeroute,
    Horizontal,
    Image,
    HostCad,
    HostVersion,
    Keepout,
    Layer,
    LayerRule,
    Length,
    LibraryScope,
    LockType,
    LogicalPart,
    LogicalPartMapping,
    Net,
    NetworkScope,
    NetworkOut,
    NinetyDegree,
    None,
    Normal,
    Off,
    On,
    Order,
    Outline,
    Padstack,
    ParserScope,
    PartLibraryScope,
    PolygonPath,
    PcbScope,
    Pin,
    Pins,
    Place,
    PlaceControl,
    PlaceKeepout,
    PlacementScope,
    PlaneScope,
    PlaneViaCosts,
    Polygon,
    PolylinePath,
    Position,
    Postroute,
    Power,
    PreferredDirection,
    PreferredDirectionTraceCosts,
    PullTight,
    Rectangle,
    ResolutionScope,
    Rotate,
    RotateFirst,
    Routes,
    Rule,
    Rules,
    Session,
    Shape,
    ShoveFixed,
    Side,
    Signal,
    SnapAngle,
    Spare,
    StartPassNo,
    StartRipupCosts,
    StringQuote,
    StructureScope,
    Type,
    UseLayer,
    UseNet,
    UseVia,
    Vertical,
    Via,
    Vias,
    ViaAtSmd,
    ViaCosts,
    ViaKeepout,
    ViaRule,
    Width,
    Window,
    Wire,
    WiringScope,
    WriteResolution,
}

impl Kw {
    /// The lexical state this keyword switches the scanner into, if any.
    ///
    /// Transcribed from the `yybegin(...)` calls in `SpecctraFileDescription.flex`. Note that
    /// `wire_keepout` maps onto [`Kw::Keepout`] **without** entering `NAME`, unlike `keepout`.
    fn entered_state(self, spelling: &str) -> Option<LexState> {
        match self {
            Kw::Circle | Kw::PolygonPath | Kw::Polygon | Kw::PolylinePath | Kw::Rectangle => {
                Some(LexState::LayerName)
            }
            Kw::StringQuote => Some(LexState::IgnoreQuote),
            Kw::Keepout if spelling == "wire_keepout" => None,
            Kw::Class
            | Kw::ClearanceClass
            | Kw::ComponentScope
            | Kw::Image
            | Kw::HostCad
            | Kw::HostVersion
            | Kw::Keepout
            | Kw::Layer
            | Kw::LayerRule
            | Kw::LogicalPart
            | Kw::LogicalPartMapping
            | Kw::Net
            | Kw::Padstack
            | Kw::Place
            | Kw::PlaceKeepout
            | Kw::PlaneScope
            | Kw::UseLayer
            | Kw::UseNet
            | Kw::UseVia
            | Kw::Via
            | Kw::ViaKeepout
            | Kw::Wire => Some(LexState::Name),
            _ => None,
        }
    }

    /// Resolves an already-lowercased identifier spelling to a keyword.
    fn from_lowercase(text: &str) -> Option<Kw> {
        Some(match text {
            "absolute" => Kw::Absolute,
            "active" => Kw::Active,
            "against_preferred_direction_trace_costs"
            | "against_prefered_direction_trace_costs" => Kw::AgainstPreferredDirectionTraceCosts,
            "attach" => Kw::Attach,
            "autoroute" => Kw::Autoroute,
            "autoroute_settings" => Kw::AutorouteSettings,
            "back" => Kw::Back,
            "boundary" => Kw::Boundary,
            "circ" | "circle" => Kw::Circle,
            "circuit" => Kw::Circuit,
            "class" => Kw::Class,
            "class_class" => Kw::ClassClass,
            "classes" => Kw::Classes,
            "clear" | "clearance" => Kw::Clearance,
            "clearance_class" => Kw::ClearanceClass,
            "comp" | "component" => Kw::ComponentScope,
            "constant" => Kw::Constant,
            "control" => Kw::Control,
            "fanout" => Kw::Fanout,
            "fix" => Kw::Fix,
            "fortyfive_degree" => Kw::FortyfiveDegree,
            "flip_style" => Kw::FlipStyle,
            "fromto" => Kw::Fromto,
            "front" => Kw::Front,
            "generated_by_freeroute" => Kw::GeneratedByFreeroute,
            "horizontal" => Kw::Horizontal,
            "image" => Kw::Image,
            "host_cad" => Kw::HostCad,
            "host_version" => Kw::HostVersion,
            "keepout" | "wire_keepout" => Kw::Keepout,
            "layer" => Kw::Layer,
            "layer_rule" => Kw::LayerRule,
            "length" => Kw::Length,
            "library" => Kw::LibraryScope,
            "lock_type" => Kw::LockType,
            "logical_part" => Kw::LogicalPart,
            "logical_part_mapping" => Kw::LogicalPartMapping,
            "net" => Kw::Net,
            "network" => Kw::NetworkScope,
            "network_out" => Kw::NetworkOut,
            "ninety_degree" => Kw::NinetyDegree,
            "none" => Kw::None,
            "normal" => Kw::Normal,
            "off" => Kw::Off,
            "on" => Kw::On,
            "order" => Kw::Order,
            "outline" => Kw::Outline,
            "padstack" => Kw::Padstack,
            "parser" => Kw::ParserScope,
            "part_library" => Kw::PartLibraryScope,
            "path" => Kw::PolygonPath,
            "pcb" => Kw::PcbScope,
            "pin" => Kw::Pin,
            "pins" => Kw::Pins,
            "place" => Kw::Place,
            "place_control" => Kw::PlaceControl,
            "place_keepout" => Kw::PlaceKeepout,
            "placement" => Kw::PlacementScope,
            "plane" => Kw::PlaneScope,
            "plane_via_costs" => Kw::PlaneViaCosts,
            "poly" | "polygon" => Kw::Polygon,
            "polyline_path" => Kw::PolylinePath,
            "position" => Kw::Position,
            "postroute" => Kw::Postroute,
            "power" => Kw::Power,
            "preferred_direction" | "prefered_direction" => Kw::PreferredDirection,
            "preferred_direction_trace_costs" | "prefered_direction_trace_costs" => {
                Kw::PreferredDirectionTraceCosts
            }
            "pull_tight" => Kw::PullTight,
            "rect" | "rectangle" => Kw::Rectangle,
            "resolution" => Kw::ResolutionScope,
            "rotate" => Kw::Rotate,
            "rotate_first" => Kw::RotateFirst,
            "routes" => Kw::Routes,
            "rule" => Kw::Rule,
            "rules" => Kw::Rules,
            "session" => Kw::Session,
            "shape" => Kw::Shape,
            "shove_fixed" => Kw::ShoveFixed,
            "side" => Kw::Side,
            "signal" => Kw::Signal,
            "snap_angle" => Kw::SnapAngle,
            "spare" => Kw::Spare,
            "start_pass_no" => Kw::StartPassNo,
            "start_ripup_costs" => Kw::StartRipupCosts,
            "string_quote" => Kw::StringQuote,
            "structure" => Kw::StructureScope,
            "type" => Kw::Type,
            "use_layer" => Kw::UseLayer,
            "use_net" => Kw::UseNet,
            "use_via" => Kw::UseVia,
            "vertical" => Kw::Vertical,
            "via" => Kw::Via,
            "vias" => Kw::Vias,
            "via_at_smd" => Kw::ViaAtSmd,
            "via_costs" => Kw::ViaCosts,
            "via_keepout" => Kw::ViaKeepout,
            "via_rule" => Kw::ViaRule,
            "width" => Kw::Width,
            "window" => Kw::Window,
            "wire" => Kw::Wire,
            "wiring" => Kw::WiringScope,
            "write_resolution" => Kw::WriteResolution,
            _ => return None,
        })
    }
}

/// One token.
///
/// [`Token::Int`] / [`Token::Float`] mirror the boxed `Integer` / `Double` upstream returns from
/// `nextToken`, and carry the **source text** so that a consumer which would have called
/// `toString()` gets the spelling back rather than a re-formatted number.
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    /// `(`
    Open,
    /// `)`
    Close,
    /// A recognized keyword.
    Kw(Kw),
    /// An identifier or a quoted string — upstream's `instanceof String`.
    Str(String),
    /// `DecIntegerLiteral`: `[+-]?(0|[1-9][0-9]*)`, with its source text.
    Int(i64, String),
    /// `DecFloatLiteral`, with its source text. Note that `007` lands here, not in
    /// [`Token::Int`], because `DecIntegerLiteral` forbids leading zeros while `Mantissa`
    /// allows them and JFlex takes the longest match.
    Float(f64, String),
}

impl Token {
    /// True for the tokens upstream tests with `instanceof String`.
    pub fn is_string(&self) -> bool {
        matches!(self, Token::Str(_))
    }

    /// True for the tokens upstream tests with `instanceof Integer`.
    pub fn is_int(&self) -> bool {
        matches!(self, Token::Int(_, _))
    }

    /// The numeric value for `Integer`/`Double` tokens — upstream's
    /// `instanceof Double d ? d : (instanceof Integer i ? i : fail)` chain.
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Token::Int(v, _) => Some(*v as f64),
            Token::Float(v, _) => Some(*v),
            _ => None,
        }
    }

    /// The `Integer` value, for the places upstream requires an integer specifically
    /// (`(resolution um 10)`, the subnet number in `(net X 2)`).
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Token::Int(v, _) => Some(*v),
            _ => None,
        }
    }

    /// Upstream's `nextToken.toString()` on a value token: the **source spelling**.
    ///
    /// Parity note: Java would print `Integer`/`Double` canonically, so `+5` becomes `5` and
    /// `7.50` becomes `7.5`. Every call site that matters (`Package.readPinInfo`'s padstack and
    /// pin names, `Component.readPlaceScope`'s component name) forces [`LexState::Name`] first,
    /// where such a token is a [`Token::Str`] and the two agree; keeping the spelling elsewhere
    /// is strictly more faithful to the file.
    pub fn as_java_string(&self) -> Option<&str> {
        match self {
            Token::Str(s) | Token::Int(_, s) | Token::Float(_, s) => Some(s),
            _ => None,
        }
    }
}

/// `Limits.CRIT_INT` — `2^25`, the bound the scale-factor loop keeps coordinates under.
pub const CRIT_INT: f64 = 33_554_432.0;

/// The tokenizer.
///
/// Owns the input bytes (DSN is ASCII-dominant; non-ASCII bytes are treated as identifier
/// characters, matching the grammar's `SpecCharANSI` class, so UTF-8 net names survive without a
/// decode step) plus the two pieces of scanner state that matter: the read position — upstream's
/// `zzMarkedPos`, the only position the DFA restarts from — and the lexical state.
pub struct Lexer<'a> {
    src: &'a [u8],
    /// `zzMarkedPos`.
    pos: usize,
    state: LexState,
    /// `SpecctraDsnStreamReader.scopeIdentifier`, used only in diagnostics. A field here rather
    /// than the `static` upstream uses, which two concurrent reads would corrupt.
    scope_identifier: String,
}

impl<'a> Lexer<'a> {
    /// Wraps a DSN source buffer.
    pub fn new(src: &'a [u8]) -> Self {
        Lexer {
            src,
            pos: 0,
            state: LexState::Initial,
            scope_identifier: String::new(),
        }
    }

    /// Upstream `yybegin`.
    pub fn begin(&mut self, state: LexState) {
        self.state = state;
    }

    /// The current scope identifier, for diagnostics.
    pub fn scope_identifier(&self) -> &str {
        &self.scope_identifier
    }

    /// Upstream `setScopeIdentifier`.
    pub fn set_scope_identifier(&mut self, name: &str) {
        self.scope_identifier.clear();
        self.scope_identifier.push_str(name);
    }

    /// True once the whole buffer has been consumed.
    pub fn at_end(&self) -> bool {
        self.pos >= self.src.len()
    }

    #[inline]
    fn byte_at(&self, index: usize) -> Option<u8> {
        self.src.get(index).copied()
    }

    /// The next token, or `None` at end of input — upstream's `nextToken`.
    pub fn next_token(&mut self) -> Option<Token> {
        let state = self.state;
        match state {
            LexState::SpecChar => {
                // <SPEC_CHAR> { {SpecChar2} { return yytext(); } } — exactly one character, no
                // whitespace rule, no state reset in the grammar. The single caller
                // (`Network.readNetPins`) hands control straight back to `next_string`.
                let byte = self.byte_at(self.pos)?;
                self.pos += 1;
                self.state = LexState::Initial;
                Some(Token::Str(String::from_utf8_lossy(&[byte]).into_owned()))
            }
            LexState::IgnoreQuote => {
                self.skip_whitespace(false);
                match self.byte_at(self.pos)? {
                    b'(' => {
                        self.pos += 1;
                        self.state = LexState::Initial;
                        Some(Token::Open)
                    }
                    b')' => {
                        self.pos += 1;
                        self.state = LexState::Initial;
                        Some(Token::Close)
                    }
                    _ => {
                        let start = self.pos;
                        while let Some(b) = self.byte_at(self.pos) {
                            if is_identifier_ignoring_quotes(b) {
                                self.pos += 1;
                            } else {
                                break;
                            }
                        }
                        self.state = LexState::Initial;
                        if self.pos == start {
                            // A zero-length match; the grammar would error out. Consume the
                            // character so the caller cannot spin.
                            self.pos += 1;
                            return Some(Token::Str(String::new()));
                        }
                        Some(Token::Str(self.text(start, self.pos)))
                    }
                }
            }
            LexState::Name | LexState::LayerName | LexState::ComponentName => {
                self.skip_whitespace(false);
                let byte = self.byte_at(self.pos)?;
                match byte {
                    b'(' => {
                        self.pos += 1;
                        self.state = LexState::Initial;
                        Some(Token::Open)
                    }
                    b')' => {
                        self.pos += 1;
                        self.state = LexState::Initial;
                        Some(Token::Close)
                    }
                    b'"' => {
                        self.pos += 1;
                        let s = self.read_dfa_quoted(b'"');
                        self.state = LexState::Initial;
                        Some(Token::Str(s))
                    }
                    b'\'' => {
                        self.pos += 1;
                        let s = self.read_dfa_quoted(b'\'');
                        self.state = LexState::Initial;
                        Some(Token::Str(s))
                    }
                    _ => {
                        let component = state == LexState::ComponentName;
                        let start = self.pos;
                        if !name_identifier_start(byte, component) {
                            // The grammar's error fallback. Skip the byte rather than panicking.
                            self.pos += 1;
                            self.state = LexState::Initial;
                            return self.next_token();
                        }
                        self.pos += 1;
                        while let Some(b) = self.byte_at(self.pos) {
                            if name_identifier_body(b, component) {
                                self.pos += 1;
                            } else {
                                break;
                            }
                        }
                        let text = self.text(start, self.pos);
                        self.state = LexState::Initial;
                        if state == LexState::LayerName {
                            // <LAYER_NAME> resolves exactly two keywords, and `Shape.getLayer`
                            // depends on it.
                            let lower = text.to_ascii_lowercase();
                            if lower == "pcb" {
                                return Some(Token::Kw(Kw::PcbScope));
                            }
                            if lower == "signal" {
                                return Some(Token::Kw(Kw::Signal));
                            }
                        }
                        Some(Token::Str(text))
                    }
                }
            }
            LexState::Initial => self.next_token_initial(),
        }
    }

    fn next_token_initial(&mut self) -> Option<Token> {
        loop {
            self.skip_whitespace(true);
            let byte = self.byte_at(self.pos)?;
            match byte {
                b'(' => {
                    self.pos += 1;
                    return Some(Token::Open);
                }
                b')' => {
                    self.pos += 1;
                    return Some(Token::Close);
                }
                b'"' => {
                    self.pos += 1;
                    return Some(Token::Str(self.read_dfa_quoted(b'"')));
                }
                b'\'' => {
                    self.pos += 1;
                    return Some(Token::Str(self.read_dfa_quoted(b'\'')));
                }
                _ => {}
            }

            // JFlex takes the longest match over the whole ruleset. `Identifier` cannot start
            // with a digit while `Mantissa` can, so the two never both match from the same
            // position with an alphabetic lead; a numeric lead can produce a number followed by
            // an identifier (`12abc` → `12`, `abc`), which is what the length comparison below
            // reproduces.
            let number_end = self.scan_number(self.pos);
            let ident_end = self.scan_identifier(self.pos);
            let end = number_end.max(ident_end);
            if end == self.pos {
                // The grammar's error fallback. Skip and retry instead of throwing.
                self.pos += 1;
                continue;
            }
            let start = self.pos;
            self.pos = end;
            if ident_end >= number_end && ident_end > start {
                let text = self.text(start, end);
                let lower = text.to_ascii_lowercase();
                if let Some(kw) = Kw::from_lowercase(&lower) {
                    if let Some(next_state) = kw.entered_state(&lower) {
                        self.state = next_state;
                    }
                    return Some(Token::Kw(kw));
                }
                return Some(Token::Str(text));
            }
            let text = self.text(start, end);
            return Some(parse_number_token(text));
        }
    }

    /// The DFA's `<STRING1>` / `<STRING2>` states: append every character until the matching
    /// quote, with `\` appended literally (there is no escape processing anywhere in the
    /// grammar). Newlines are ordinary characters here — unlike in [`Lexer::next_string`].
    fn read_dfa_quoted(&mut self, quote: u8) -> String {
        let start = self.pos;
        while let Some(b) = self.byte_at(self.pos) {
            if b == quote {
                break;
            }
            self.pos += 1;
        }
        let text = self.text(start, self.pos);
        if self.byte_at(self.pos) == Some(quote) {
            self.pos += 1;
        }
        text
    }

    /// Skips whitespace, and — only in `YYINITIAL`, where the grammar puts the `Comment` rule —
    /// `#`-to-end-of-line and `/* … */` comments.
    fn skip_whitespace(&mut self, comments: bool) {
        loop {
            match self.byte_at(self.pos) {
                Some(b) if b.is_ascii_whitespace() || b == 0x0c => self.pos += 1,
                Some(b'#') if comments => {
                    while let Some(b) = self.byte_at(self.pos) {
                        self.pos += 1;
                        if b == b'\n' {
                            break;
                        }
                    }
                }
                Some(b'/') if comments && self.byte_at(self.pos + 1) == Some(b'*') => {
                    self.pos += 2;
                    while self.pos < self.src.len() {
                        if self.byte_at(self.pos) == Some(b'*')
                            && self.byte_at(self.pos + 1) == Some(b'/')
                        {
                            self.pos += 2;
                            break;
                        }
                        self.pos += 1;
                    }
                }
                _ => return,
            }
        }
    }

    /// End position of the longest `Identifier` match starting at `from`, or `from` if none.
    fn scan_identifier(&self, from: usize) -> usize {
        let Some(first) = self.byte_at(from) else {
            return from;
        };
        if !identifier_start(first) {
            return from;
        }
        let mut end = from + 1;
        while let Some(b) = self.byte_at(end) {
            if identifier_body(b) {
                end += 1;
            } else {
                break;
            }
        }
        end
    }

    /// End position of the longest `DecIntegerLiteral | DecFloatLiteral` match starting at
    /// `from`, or `from` if none.
    fn scan_number(&self, from: usize) -> usize {
        let mut cursor = from;
        if matches!(self.byte_at(cursor), Some(b'+') | Some(b'-')) {
            cursor += 1;
        }
        let digits_start = cursor;
        while matches!(self.byte_at(cursor), Some(b) if b.is_ascii_digit()) {
            cursor += 1;
        }
        if cursor == digits_start {
            return from;
        }
        let mut end = cursor;
        // Optional fraction, then optional exponent — both all-or-nothing.
        if self.byte_at(cursor) == Some(b'.') {
            let mut fraction = cursor + 1;
            let fraction_start = fraction;
            while matches!(self.byte_at(fraction), Some(b) if b.is_ascii_digit()) {
                fraction += 1;
            }
            if fraction > fraction_start {
                end = fraction;
            }
        }
        let mut exponent = end;
        if matches!(self.byte_at(exponent), Some(b'e') | Some(b'E')) {
            exponent += 1;
            if matches!(self.byte_at(exponent), Some(b'+') | Some(b'-')) {
                exponent += 1;
            }
            let exponent_start = exponent;
            while matches!(self.byte_at(exponent), Some(b) if b.is_ascii_digit()) {
                exponent += 1;
            }
            if exponent > exponent_start {
                end = exponent;
            }
        }
        end
    }

    fn text(&self, start: usize, end: usize) -> String {
        String::from_utf8_lossy(&self.src[start..end]).into_owned()
    }

    /// Upstream `nextString()` — the raw-buffer bypass. `ignore_newline` adds LF and CR to the
    /// skipped leading characters; `leading` is an extra character that both terminates the skip
    /// set and the stop set (`'-'` for `(pins U1-3 …)`).
    ///
    /// Returns `""` at exhaustion and never advances past a stop character, which is how
    /// [`Lexer::next_string_list`] terminates. The transcription is deliberately literal,
    /// including the fact that a horizontal tab is in **neither** set and therefore ends up
    /// inside the returned string.
    pub fn next_string(&mut self, ignore_newline: bool, leading: u8) -> String {
        let mut out = String::new();
        let mut i = 0usize;

        let skip_leading = |b: u8| -> bool {
            let base = b == 8 || b == 32 || (ignore_newline && (b == 10 || b == 13));
            base || b == leading
        };
        while let Some(b) = self.byte_at(self.pos + i) {
            if skip_leading(b) {
                i += 1;
            } else {
                break;
            }
        }

        let mut skip_last_char = false;
        let quoted = self.byte_at(self.pos + i) == Some(34);
        if quoted {
            i += 1;
            skip_last_char = true;
        }
        let stop_at = |b: u8| -> bool {
            if quoted {
                b == 10 || b == 13 || b == 34
            } else {
                b == 8 || b == 10 || b == 13 || b == 32 || b == 40 || b == 41 || b == leading
            }
        };

        let value_start = self.pos + i;
        while let Some(b) = self.byte_at(self.pos + i) {
            if stop_at(b) {
                break;
            }
            i += 1;
        }
        out.push_str(&String::from_utf8_lossy(
            &self.src[value_start..self.pos + i],
        ));

        if skip_last_char {
            i += 1;
        }
        if i > 0 {
            self.pos += i;
            self.state = LexState::Initial;
            if matches!(self.byte_at(self.pos.wrapping_sub(1)), Some(40) | Some(41)) {
                // The string ran into a bracket; hand the bracket back as the next token.
                self.pos -= 1;
            }
        }
        out
    }

    /// Upstream `nextString(false, ' ')`.
    pub fn next_string_default(&mut self) -> String {
        self.next_string(false, b' ')
    }

    /// Upstream `nextStringList(separator)`.
    ///
    /// The leading-empty tolerance is a KiCad 8 workaround: it exports
    /// `(class kicad_default "" NET1 …)`, and dropping that empty element is the only reason
    /// those boards get their net-class membership right.
    pub fn next_string_list(&mut self, separator: u8) -> Vec<String> {
        let mut result = Vec::new();
        let first = self.next_string(true, separator);
        if !first.is_empty() {
            result.push(first);
        }
        loop {
            let next = self.next_string(true, separator);
            if next.is_empty() {
                break;
            }
            result.push(next);
        }
        result
    }

    /// Upstream `nextDouble()`: `nextString()` parsed with a US `NumberFormat`.
    ///
    /// `NumberFormat.parse` stops at the first unparseable character rather than failing, so
    /// `12abc` yields `12`; that leniency is reproduced here.
    pub fn next_double(&mut self) -> Option<f64> {
        let s = self.next_string_default();
        parse_leading_number(&s)
    }

    /// Upstream `nextClosingBracket()`: consume one token and report whether it was `)`.
    pub fn next_closing_bracket(&mut self) -> bool {
        matches!(self.next_token(), Some(Token::Close))
    }
}

/// Java `NumberFormat.getInstance(Locale.US).parse` for the subset DSN uses: an optional sign,
/// digits, an optional `.` fraction and an optional exponent, stopping at the first character
/// that cannot extend the number.
fn parse_leading_number(text: &str) -> Option<f64> {
    let bytes = text.as_bytes();
    let mut end = 0usize;
    if matches!(bytes.first(), Some(b'+') | Some(b'-')) {
        end = 1;
    }
    let digits_start = end;
    while matches!(bytes.get(end), Some(b) if b.is_ascii_digit()) {
        end += 1;
    }
    if end == digits_start {
        return None;
    }
    if bytes.get(end) == Some(&b'.') {
        let mut fraction = end + 1;
        let fraction_start = fraction;
        while matches!(bytes.get(fraction), Some(b) if b.is_ascii_digit()) {
            fraction += 1;
        }
        if fraction > fraction_start {
            end = fraction;
        }
    }
    let mut exponent = end;
    if matches!(bytes.get(exponent), Some(b'e') | Some(b'E')) {
        exponent += 1;
        if matches!(bytes.get(exponent), Some(b'+') | Some(b'-')) {
            exponent += 1;
        }
        let exponent_start = exponent;
        while matches!(bytes.get(exponent), Some(b) if b.is_ascii_digit()) {
            exponent += 1;
        }
        if exponent > exponent_start {
            end = exponent;
        }
    }
    text[..end].parse::<f64>().ok()
}

/// Splits a numeric literal into [`Token::Int`] or [`Token::Float`] the way the grammar does:
/// `DecIntegerLiteral` is `[+-]?(0|[1-9][0-9]*)` and loses the longest-match race against
/// `DecFloatLiteral` for anything longer, which is why `007` is a `Double`.
fn parse_number_token(text: String) -> Token {
    let body = text
        .strip_prefix(['+', '-'])
        .map_or(text.as_str(), |rest| rest);
    let is_dec_integer = !body.is_empty()
        && body.bytes().all(|b| b.is_ascii_digit())
        && (body == "0" || !body.starts_with('0'));
    if is_dec_integer {
        if let Ok(value) = text.parse::<i64>() {
            return Token::Int(value, text);
        }
    }
    let value = text.parse::<f64>().unwrap_or(0.0);
    Token::Float(value, text)
}

/// `SpecCharASCII` from the grammar, plus every non-ASCII byte (`SpecCharANSI`).
fn is_spec_char1(b: u8) -> bool {
    matches!(
        b,
        b'_' | b'.'
            | b'/'
            | b'\\'
            | b':'
            | b'#'
            | b'$'
            | b'&'
            | b'>'
            | b'<'
            | b','
            | b';'
            | b'='
            | b'@'
            | b'['
            | b']'
            | b'~'
            | b'*'
            | b'?'
            | b'!'
            | b'%'
            | b'^'
    ) || b >= 0x80
}

/// `SpecChar3 = SpecChar1 | - | + | " | '`.
fn is_spec_char3(b: u8) -> bool {
    is_spec_char1(b) || matches!(b, b'-' | b'+' | b'"' | b'\'')
}

/// `SpecChar5 = SpecChar1 | + | " | '` — the `-`-free variant used by `ComponentIdentifier`.
fn is_spec_char5(b: u8) -> bool {
    is_spec_char1(b) || matches!(b, b'+' | b'"' | b'\'')
}

fn identifier_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || is_spec_char1(b)
}

fn identifier_body(b: u8) -> bool {
    b.is_ascii_alphanumeric() || is_spec_char3(b)
}

fn name_identifier_start(b: u8, component: bool) -> bool {
    if component {
        b.is_ascii_alphanumeric() || is_spec_char1(b) || b == b'+'
    } else {
        b.is_ascii_alphanumeric() || is_spec_char1(b) || b == b'-' || b == b'+'
    }
}

fn name_identifier_body(b: u8, component: bool) -> bool {
    if component {
        b.is_ascii_alphanumeric() || is_spec_char5(b)
    } else {
        b.is_ascii_alphanumeric() || is_spec_char3(b)
    }
}

fn is_identifier_ignoring_quotes(b: u8) -> bool {
    b.is_ascii_alphanumeric() || is_spec_char3(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(src: &str) -> Lexer<'_> {
        Lexer::new(src.as_bytes())
    }

    #[test]
    fn numbers_keep_their_spelling_and_java_type() {
        let mut lexer = lex("0 007 12 -3 1.5 1e3 +5");
        assert_eq!(lexer.next_token(), Some(Token::Int(0, "0".into())));
        // 007 loses the longest-match race for DecIntegerLiteral and becomes a Double.
        assert_eq!(lexer.next_token(), Some(Token::Float(7.0, "007".into())));
        assert_eq!(lexer.next_token(), Some(Token::Int(12, "12".into())));
        assert_eq!(lexer.next_token(), Some(Token::Int(-3, "-3".into())));
        assert_eq!(lexer.next_token(), Some(Token::Float(1.5, "1.5".into())));
        assert_eq!(lexer.next_token(), Some(Token::Float(1000.0, "1e3".into())));
        assert_eq!(lexer.next_token(), Some(Token::Int(5, "+5".into())));
    }

    #[test]
    fn name_state_keeps_numeric_identifiers() {
        let mut lexer = lex("1 007 12");
        lexer.begin(LexState::Name);
        assert_eq!(lexer.next_token(), Some(Token::Str("1".into())));
        // The state reverts after one token, exactly like `yybegin(NAME)`.
        assert_eq!(lexer.next_token(), Some(Token::Float(7.0, "007".into())));
        lexer.begin(LexState::Name);
        assert_eq!(lexer.next_token(), Some(Token::Str("12".into())));
    }

    #[test]
    fn keywords_are_case_insensitive_and_lose_to_longer_identifiers() {
        let mut lexer = lex("(PCB Front via_something wire_keepout)");
        assert_eq!(lexer.next_token(), Some(Token::Open));
        assert_eq!(lexer.next_token(), Some(Token::Kw(Kw::PcbScope)));
        assert_eq!(lexer.next_token(), Some(Token::Kw(Kw::Front)));
        assert_eq!(lexer.next_token(), Some(Token::Str("via_something".into())));
        // wire_keepout maps onto KEEPOUT but does not enter NAME.
        assert_eq!(lexer.next_token(), Some(Token::Kw(Kw::Keepout)));
        assert_eq!(lexer.state, LexState::Initial);
    }

    #[test]
    fn keyword_next_token_is_a_name() {
        let mut lexer = lex("(net 1)");
        assert_eq!(lexer.next_token(), Some(Token::Open));
        assert_eq!(lexer.next_token(), Some(Token::Kw(Kw::Net)));
        assert_eq!(lexer.next_token(), Some(Token::Str("1".into())));
        assert_eq!(lexer.next_token(), Some(Token::Close));
    }

    #[test]
    fn layer_name_state_resolves_only_pcb_and_signal() {
        let mut lexer = lex("(path pcb 0 1 2) (path B.Cu 0) (path 12 0)");
        assert_eq!(lexer.next_token(), Some(Token::Open));
        assert_eq!(lexer.next_token(), Some(Token::Kw(Kw::PolygonPath)));
        assert_eq!(lexer.next_token(), Some(Token::Kw(Kw::PcbScope)));
        // 0, 1, 2, ')' and the next '('.
        for _ in 0..5 {
            lexer.next_token();
        }
        assert_eq!(lexer.next_token(), Some(Token::Kw(Kw::PolygonPath)));
        assert_eq!(lexer.next_token(), Some(Token::Str("B.Cu".into())));
        for _ in 0..3 {
            lexer.next_token();
        }
        assert_eq!(lexer.next_token(), Some(Token::Kw(Kw::PolygonPath)));
        assert_eq!(lexer.next_token(), Some(Token::Str("12".into())));
    }

    #[test]
    fn string_quote_directive_reads_the_quote_character_as_an_identifier() {
        let mut lexer = lex("(string_quote \")");
        assert_eq!(lexer.next_token(), Some(Token::Open));
        assert_eq!(lexer.next_token(), Some(Token::Kw(Kw::StringQuote)));
        assert_eq!(lexer.next_token(), Some(Token::Str("\"".into())));
        assert_eq!(lexer.next_token(), Some(Token::Close));
    }

    #[test]
    fn next_string_quoted_value_terminates_at_end_of_line() {
        // KiCad writes unescaped inch marks inside part numbers; upstream survives because the
        // bypass path stops the string at the newline instead of running away.
        let mut lexer = lex("  \"1/4\" screw\nnext");
        assert_eq!(lexer.next_string(true, b' '), "1/4");
        assert_eq!(lexer.next_string(true, b' '), "screw");
        assert_eq!(lexer.next_string(true, b' '), "next");
    }

    #[test]
    fn next_string_stops_before_brackets_and_returns_empty_at_a_close() {
        let mut lexer = lex(" NET1 NET2 )");
        assert_eq!(lexer.next_string(true, b' '), "NET1");
        assert_eq!(lexer.next_string(true, b' '), "NET2");
        assert_eq!(lexer.next_string(true, b' '), "");
        assert_eq!(lexer.next_token(), Some(Token::Close));
    }

    #[test]
    fn next_string_list_tolerates_the_kicad8_leading_empty_element() {
        let mut lexer = lex(" \"\" NET1 NET2)");
        assert_eq!(lexer.next_string_list(b' '), vec!["NET1", "NET2"]);
        assert_eq!(lexer.next_token(), Some(Token::Close));
    }

    #[test]
    fn next_string_list_splits_clearance_pairs_on_hyphen() {
        let mut lexer = lex(" smd-via)");
        assert_eq!(lexer.next_string_list(b'-'), vec!["smd", "via"]);
        let mut lexer = lex(" smd_via)");
        assert_eq!(lexer.next_string_list(b'-'), vec!["smd_via"]);
    }

    #[test]
    fn net_pin_names_split_on_the_hyphen_through_spec_char() {
        let mut lexer = lex(" U1-3 \"MY-PART\"-A1)");
        assert_eq!(lexer.next_string(true, b'-'), "U1");
        lexer.begin(LexState::SpecChar);
        assert_eq!(lexer.next_token(), Some(Token::Str("-".into())));
        assert_eq!(lexer.next_string(true, b' '), "3");
        // A quoted component name may contain the separator.
        assert_eq!(lexer.next_string(true, b'-'), "MY-PART");
        lexer.begin(LexState::SpecChar);
        assert_eq!(lexer.next_token(), Some(Token::Str("-".into())));
        assert_eq!(lexer.next_string(true, b' '), "A1");
        assert_eq!(lexer.next_string(true, b'-'), "");
    }

    #[test]
    fn dfa_quoted_strings_span_newlines_and_never_unescape() {
        let mut lexer = lex("\"a\\b\nc\" rest");
        assert_eq!(lexer.next_token(), Some(Token::Str("a\\b\nc".into())));
        assert_eq!(lexer.next_token(), Some(Token::Str("rest".into())));
    }

    #[test]
    fn comments_are_skipped_only_in_the_initial_state() {
        let mut lexer = lex("# a comment\n(pcb x)");
        assert_eq!(lexer.next_token(), Some(Token::Open));
        assert_eq!(lexer.next_token(), Some(Token::Kw(Kw::PcbScope)));
    }

    #[test]
    fn non_ascii_identifiers_survive() {
        let mut lexer = lex("(net Паяльная)");
        assert_eq!(lexer.next_token(), Some(Token::Open));
        assert_eq!(lexer.next_token(), Some(Token::Kw(Kw::Net)));
        assert_eq!(lexer.next_token(), Some(Token::Str("Паяльная".into())));
    }
}
