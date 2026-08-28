//! Conformance tests for the Specctra tokenizer.
//!
//! The unit tests inside `specctra::lex` cover the token shapes; these cover the *interactions*
//! that `dsn-board-semantics.md` §2 and §14 call out as the ones a minimal parser gets wrong,
//! driven through the real scope readers so a regression in either half shows up here.

use fr_io::specctra::{parse, Kw, LexState, Lexer, ReadOutcome, Token};

fn ast(src: &str) -> Box<fr_io::specctra::DsnAst> {
    match parse(src.as_bytes()).0 {
        ReadOutcome::Ok(ast) => ast,
        other => panic!("expected a parse, got {other:?}"),
    }
}

/// §14.1 — the single most common failure mode: a parser that always lexes numbers as numbers
/// loses every numerically-named pin, net, class, layer and design name.
#[test]
fn numeric_names_survive_everywhere_the_reader_forces_name_mode() {
    let src = r#"(pcb 42
  (resolution um 10)
  (structure
    (layer 12 (type signal))
    (layer 13 (type signal))
    (boundary (rect pcb 0 0 1000 1000))
  )
  (library
    (image 7
      (pin 5 1 0 0)
      (pin 5 007 100 0)
    )
    (padstack 5 (shape (rect 12 -10 -10 10 10)))
  )
  (network
    (net 1 (pins 9-1 9-007))
    (class 3 1)
  )
)"#;
    let ast = ast(src);
    assert_eq!(ast.design_name, "42");
    assert_eq!(ast.structure.layers[0].name, "12");
    assert_eq!(ast.structure.layers[1].name, "13");
    assert_eq!(ast.packages[0].name, "7");
    assert_eq!(ast.packages[0].pins[0].padstack_name, "5");
    assert_eq!(ast.packages[0].pins[0].pin_name, "1");
    // `007` is a `Double` in YYINITIAL because `DecIntegerLiteral` forbids leading zeros; the
    // forced NAME state is what keeps the spelling.
    assert_eq!(ast.packages[0].pins[1].pin_name, "007");
    assert_eq!(ast.padstacks[0].name, "5");
    assert_eq!(ast.nets[0].name, "1");
    assert_eq!(ast.nets[0].pins[0].component_name, "9");
    assert_eq!(ast.nets[0].pins[0].pin_name, "1");
    assert_eq!(ast.nets[0].pins[1].pin_name, "007");
    assert_eq!(ast.net_classes[0].name, "3");
    assert_eq!(ast.net_classes[0].net_list, vec!["1"]);
}

/// §14.4 — KiCad quotes net names that contain spaces and brackets, and writes the quote
/// character itself in `(string_quote ")`.
#[test]
fn quoted_names_with_spaces_and_brackets_round_trip() {
    let src = r#"(pcb "my board.dsn"
  (parser (string_quote ") (space_in_quoted_tokens on))
  (resolution um 10)
  (structure (layer F.Cu (type signal)) (boundary (rect pcb 0 0 1000 1000)))
  (network
    (net "Net-(U1-Pad3)" (pins "MY-PART"-3))
    (class kicad_default "" "Net-(U1-Pad3)")
  )
)"#;
    let ast = ast(src);
    assert_eq!(ast.string_quote, "\"");
    assert_eq!(ast.design_name, "my board.dsn");
    assert_eq!(ast.nets[0].name, "Net-(U1-Pad3)");
    // A component name may contain the '-' separator only when quoted.
    assert_eq!(ast.nets[0].pins[0].component_name, "MY-PART");
    assert_eq!(ast.nets[0].pins[0].pin_name, "3");
    // KiCad 8's leading "" in the class net list is dropped, not treated as a net.
    assert_eq!(ast.net_classes[0].net_list, vec!["Net-(U1-Pad3)"]);
}

/// The inch-mark lesson: KiCad writes unescaped `"` inside part numbers. Upstream survives only
/// because the bypass reader terminates a quoted string at the end of the line; a reader that
/// scans to the next `"` swallows the rest of the file.
#[test]
fn an_unescaped_quote_inside_a_part_number_does_not_swallow_the_file() {
    let src = "(pcb b\n\
  (resolution um 10)\n\
  (structure (layer F.Cu (type signal)) (boundary (rect pcb 0 0 1000 1000)))\n\
  (placement (component PKG (place U1 0 0 front 0 (PN \"1/4\" screw\"))))\n\
  (network (net N1 (pins U1-1)))\n\
)";
    let ast = ast(src);
    assert_eq!(
        ast.placements[0].locations[0].part_number.as_deref(),
        Some("1/4")
    );
    // The network scope after the runaway quote is still parsed.
    assert_eq!(ast.nets.len(), 1);
    assert_eq!(ast.nets[0].name, "N1");
}

/// §7.1 — `(type a-b)` and `(type a_b)` reach the clearance-rule code as lists of different
/// length, and `Structure.setClearanceRule` branches on that length.
#[test]
fn clearance_type_lists_split_on_hyphen_not_underscore() {
    let src = r#"(pcb b
  (resolution um 10)
  (structure
    (layer F.Cu (type signal))
    (boundary (rect pcb 0 0 1000 1000))
    (rule (clearance 200 (type smd-via)) (clearance 300 (type smd_via)) (clearance 400 (type "My Class"_via)))
  )
)"#;
    let ast = ast(src);
    let pairs: Vec<Vec<String>> = ast
        .structure
        .default_rules
        .iter()
        .filter_map(|rule| match rule {
            fr_io::specctra::ast::DsnRule::Clearance { class_pairs, .. } => {
                Some(class_pairs.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(pairs[0], vec!["smd", "via"]);
    assert_eq!(pairs[1], vec!["smd_via"]);
    // A quoted first name also arrives as *two* elements, because the bypass reader ends the
    // quoted string at the closing quote and the remainder `_via` is the next element. That is
    // why the `stringQuote` branch of `Structure.setClearanceRule` is nearly unreachable in v2:
    // the two-element fast path claims the case first, strips the quotes and drops the leading
    // underscore.
    assert_eq!(pairs[2], vec!["My Class", "_via"]);
}

/// §2.1 — `wire_keepout` maps onto `KEEPOUT` but, unlike `keepout`, does **not** put the scanner
/// into `NAME`. A reader that treats the two identically mis-lexes the next token.
#[test]
fn wire_keepout_is_a_keepout_that_does_not_enter_name_mode() {
    let mut lexer = Lexer::new(b"(wire_keepout (rect F.Cu 0 0 1 1))");
    assert_eq!(lexer.next_token(), Some(Token::Open));
    assert_eq!(lexer.next_token(), Some(Token::Kw(Kw::Keepout)));
    // Still YYINITIAL, so the following '(' is an ordinary bracket.
    assert_eq!(lexer.next_token(), Some(Token::Open));
    assert_eq!(lexer.next_token(), Some(Token::Kw(Kw::Rectangle)));
}

/// §2.3 — the bypass reader never advances past a stop character, which is what terminates
/// `nextStringList`; and it resets the lexical state, so a forced `NAME` does not leak.
#[test]
fn next_string_leaves_the_stream_positioned_for_the_next_token() {
    let mut lexer = Lexer::new(b"  alpha beta)(gamma");
    lexer.begin(LexState::Name);
    assert_eq!(lexer.next_string(true, b' '), "alpha");
    assert_eq!(lexer.next_string(true, b' '), "beta");
    assert_eq!(lexer.next_string(true, b' '), "");
    assert_eq!(lexer.next_token(), Some(Token::Close));
    assert_eq!(lexer.next_token(), Some(Token::Open));
    assert_eq!(lexer.next_token(), Some(Token::Str("gamma".into())));
}

/// §14.10 — `(type jumper)` arrives as a string because the lexer has no `JUMPER` keyword, and
/// the layer still counts as a signal layer.
#[test]
fn jumper_layers_are_signal_layers() {
    let src = r#"(pcb b
  (resolution um 10)
  (structure
    (layer F.Cu (type signal))
    (layer J1 (type jumper))
    (layer B.Cu (type power) (use_net GND))
    (layer BAD (type nonsense))
    (boundary (rect pcb 0 0 1000 1000))
  )
)"#;
    let ast = ast(src);
    // The unknown type drops its layer entirely, so only three layers exist and the indices of
    // the survivors are unaffected by the rejected one.
    assert_eq!(ast.structure.layers.len(), 3);
    assert!(ast.structure.layers[1].is_signal, "jumper counts as signal");
    assert!(!ast.structure.layers[2].is_signal);
    assert_eq!(ast.structure.layers[2].net_names, vec!["GND"]);
    assert_eq!(ast.structure.layers[2].no, 2);
}

/// §14.2 — `(rotate …)` is legal both before the pin name and after the coordinates.
#[test]
fn pin_rotation_is_accepted_in_both_positions() {
    let src = r#"(pcb b
  (resolution um 10)
  (structure (layer F.Cu (type signal)) (boundary (rect pcb 0 0 1000 1000)))
  (library
    (image PKG
      (pin PAD (rotate 90) A1 10 20)
      (pin PAD B2 30 40 (rotate -45.5))
    )
    (padstack PAD (shape (rect F.Cu -1 -1 1 1)))
  )
)"#;
    let ast = ast(src);
    let pins = &ast.packages[0].pins;
    assert_eq!(pins[0].pin_name, "A1");
    assert_eq!(pins[0].rotation, 90.0);
    assert_eq!(pins[0].rel_coor, [10.0, 20.0]);
    assert_eq!(pins[1].pin_name, "B2");
    assert_eq!(pins[1].rotation, -45.5);
    assert_eq!(pins[1].rel_coor, [30.0, 40.0]);
}

/// §6.6 — the `autoroute_settings` guard bug: the scope is parsed only while the layer structure
/// has not been materialized, and *any* preceding keepout materializes it.
#[test]
fn autoroute_settings_are_dropped_when_a_keepout_comes_first() {
    let head = "(pcb b\n  (resolution um 10)\n  (structure\n    (layer F.Cu (type signal))\n    (boundary (rect pcb 0 0 1000 1000))\n";
    let settings = "    (autoroute_settings (vias off) (via_costs 7))\n";
    let keepout = "    (keepout \"\" (rect F.Cu 10 10 20 20))\n";

    let before = ast(&format!("{head}{settings}{keepout}  )\n)"));
    let settings_read = before
        .structure
        .autoroute_settings
        .as_ref()
        .expect("settings before the keepout are parsed");
    assert_eq!(settings_read.vias_allowed, Some(false));
    assert_eq!(settings_read.via_costs, Some(7));
    assert_eq!(before.structure.keepouts.len(), 1);

    let after = ast(&format!("{head}{keepout}{settings}  )\n)"));
    assert!(
        after.structure.autoroute_settings.is_none(),
        "the guard drops the scope once the layer structure exists"
    );
    // …and parsing still terminates, because the unread tokens fall through as unknown scopes.
    assert_eq!(after.structure.keepouts.len(), 1);
    assert_eq!(after.structure.layers.len(), 1);
}

/// A `(class …)` scope with **no** sub-scope overshoots, and that is upstream behaviour, not a
/// port bug.
///
/// `NetClass.readScope` consumes one token after the member-net list unconditionally
/// (`NetClass.java:78`). When the class has a `(rule …)` or `(circuit …)` that token is the
/// opening bracket of the sub-scope and everything lines up; when the class is bare, the token is
/// the class's own closing bracket, so the loop that follows starts *outside* the scope and skips
/// the next sibling scope instead. Real exports always carry at least a `(rule …)`, which is why
/// nobody notices.
///
/// This is asserted rather than fixed, because fixing it would change which net classes exist on
/// any file that does hit it.
#[test]
fn a_bare_class_scope_swallows_its_sibling_upstream_and_here() {
    let bare = ast(r#"(pcb b
  (resolution um 10)
  (structure (layer F.Cu (type signal)) (boundary (rect pcb 0 0 1000 1000)))
  (network (net N1) (net N2) (class A N1) (class B N2))
)"#);
    assert_eq!(bare.net_classes.len(), 1, "the bare class ate its sibling");
    assert_eq!(bare.net_classes[0].name, "A");

    let with_rules = ast(r#"(pcb b
  (resolution um 10)
  (structure (layer F.Cu (type signal)) (boundary (rect pcb 0 0 1000 1000)))
  (network (net N1) (net N2) (class A N1 (rule (width 250))) (class B N2 (rule (width 250))))
)"#);
    let names: Vec<&str> = with_rules
        .net_classes
        .iter()
        .map(|class| class.name.as_str())
        .collect();
    assert_eq!(names, vec!["A", "B"]);
}
