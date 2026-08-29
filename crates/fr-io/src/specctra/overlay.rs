//! The **session overlay**: the board a `.ses` file must be read into to be judged.
//!
//! [`read_session`](super::ses_read::read_session) *merges* into whatever board it is handed
//! (see that module's docs), and both Freerouting and this port re-emit the input board's
//! routing in their session file. Reading a session into the full DSN import therefore inserts
//! every pre-existing trace twice — on `Issue093` that alone moves the judged clearance-violation
//! count from 192 to 322. The judged board has to be built from a DSN whose copper has been
//! removed first.
//!
//! # Remove exactly what the session carries — no more
//!
//! The obvious "empty the whole `(wiring …)` scope" is *too much*, and one fixture in the
//! upstream suite proves it. `SesWriter.writeNet` (`SesWriter.java:306`) skips every
//! `SYSTEM_FIXED` item, which is what `(type fix)` copper imports as
//! (`parser/Wiring.calc_fixed`, `dsn-board-semantics.md` §10.3). That copper is board-owned:
//! it is on the board, it shorts pins together, it takes part in clearance checks — and no
//! session file, ours or the jar's, ever mentions it. Emptying the scope hands the reader a
//! board that is missing it, and the judged result is wrong in both directions at once:
//!
//! | `Issue230-CNH_Functional_Tester_1.dsn` (the suite's only `(type fix)` board, 8 such wires) | unrouted | violations |
//! |---|---|---|
//! | our routed board, judged in process | 5 | 16 |
//! | its session, read into an emptied `(wiring)` | 8 | **14** |
//! | its session, read into [`strip_session_copper`] | 5 | 16 |
//!
//! The eight wires close three connections and are one side of two clearance violations, so
//! dropping them costs three unrouted *and* hides two violations — a judging method that
//! silently forgives violations is worse than one that invents them. (The jar's own session for
//! that board scores 2/16 through this overlay and 8/14 through the emptied one, so the error is
//! not symmetric between tools either.)
//!
//! [`strip_session_copper`] therefore removes a `(wire …)` or `(via …)` from the wiring scope
//! **unless** it is `(type fix)`, and leaves every other child of the scope alone.
//!
//! ```no_run
//! let dsn = std::fs::read("board.dsn").unwrap();
//! let ses = std::fs::read("routed.ses").unwrap();
//! let target = fr_io::specctra::strip_session_copper(&dsn);
//! let mut design = fr_io::specctra::import(&target).unwrap();
//! let library = fr_io::specctra::SesLibrary::from_design(&design);
//! let summary = fr_io::specctra::read_session(
//!     &ses,
//!     fr_io::specctra::SesTarget::for_design(&mut design, &library),
//! )
//! .unwrap();
//! assert_eq!(summary.errors_encountered, 0);
//! ```

/// Byte span `[start, end]` of the top-level `(wiring …)` scope, brackets included.
///
/// The scan starts at the **last** `(wiring` in the file. The scope is top level and cannot
/// nest, so the last occurrence is the real one even when a component or image is named after
/// it, and starting there keeps the `(string_quote ")` directive in the parser scope — whose
/// lone `"` would otherwise flip the quote state for everything after it — out of the way.
fn wiring_span(src: &[u8]) -> Option<(usize, usize)> {
    let start = (0..src.len())
        .rev()
        .find(|&index| src[index..].starts_with(b"(wiring"))?;
    let mut depth = 0i32;
    let mut in_quote = false;
    let mut end = start;
    while end < src.len() {
        match src[end] {
            b'"' => in_quote = !in_quote,
            b'(' if !in_quote => depth += 1,
            b')' if !in_quote => {
                depth -= 1;
                if depth == 0 {
                    return Some((start, end));
                }
            }
            _ => {}
        }
        end += 1;
    }
    // Unbalanced file: the reader will reject it anyway, so leave it untouched.
    None
}

fn is_atom_byte(byte: u8) -> bool {
    !byte.is_ascii_whitespace() && byte != b'(' && byte != b')' && byte != b'"'
}

/// The keyword of a scope that starts at `src[0] == b'('`, e.g. `wire` for `(wire (path …))`.
fn scope_keyword(src: &[u8]) -> &[u8] {
    let mut index = 1;
    while index < src.len() && src[index].is_ascii_whitespace() {
        index += 1;
    }
    let start = index;
    while index < src.len() && is_atom_byte(src[index]) {
        index += 1;
    }
    &src[start..index]
}

/// Does this `(wire …)` / `(via …)` carry `(type fix)`?
///
/// `parser/Wiring.calc_fixed` reads the token after `(type`: `fix` is the only spelling that
/// yields `SYSTEM_FIXED`; `shove_fixed` and `normal` are weaker and anything else (`protect`,
/// `route`) is `USER_FIXED`. All of those *are* written to a session file, so only `fix` is
/// looked for here.
fn is_system_fixed(item: &[u8]) -> bool {
    let mut index = 0;
    while index < item.len() {
        if item[index..].starts_with(b"(type") {
            let mut cursor = index + b"(type".len();
            while cursor < item.len() && item[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            let start = cursor;
            while cursor < item.len() && is_atom_byte(item[cursor]) {
                cursor += 1;
            }
            if &item[start..cursor] == b"fix" {
                return true;
            }
        }
        index += 1;
    }
    false
}

/// Removes from a DSN exactly the copper a session file re-emits, leaving `(type fix)` copper
/// and every non-copper child of the wiring scope in place.
///
/// The result is the board a `.ses` produced from this DSN should be read into; see the module
/// docs for why "empty the whole scope" is not that board. A DSN with no wiring scope is
/// returned unchanged.
pub fn strip_session_copper(src: &[u8]) -> Vec<u8> {
    let Some((start, end)) = wiring_span(src) else {
        return src.to_vec();
    };
    let body = &src[start + b"(wiring".len()..end];

    let mut kept: Vec<u8> = Vec::new();
    let mut depth = 0i32;
    let mut in_quote = false;
    let mut child_start = 0usize;
    for index in 0..body.len() {
        match body[index] {
            b'"' => in_quote = !in_quote,
            b'(' if !in_quote => {
                if depth == 0 {
                    child_start = index;
                }
                depth += 1;
            }
            b')' if !in_quote => {
                depth -= 1;
                if depth == 0 {
                    let child = &body[child_start..=index];
                    let keyword = scope_keyword(child);
                    let is_copper = keyword == b"wire" || keyword == b"via";
                    if !is_copper || is_system_fixed(child) {
                        kept.push(b'\n');
                        kept.extend_from_slice(child);
                    }
                }
            }
            _ => {}
        }
    }

    let mut out = Vec::with_capacity(src.len());
    out.extend_from_slice(&src[..start]);
    out.extend_from_slice(b"(wiring");
    out.extend_from_slice(&kept);
    out.extend_from_slice(b"\n)");
    out.extend_from_slice(&src[end + 1..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DSN: &[u8] = b"(pcb x\n(wiring\n\
        (wire (path F.Cu 635  1 -1  2 -2)(net A))\n\
        (wire (path F.Cu 635  3 -3  4 -4)(net B)(type protect))\n\
        (wire (path F.Cu 635  5 -5  6 -6)(net C)(type shove_fixed))\n\
        (wire (path F.Cu 635  7 -7  8 -8)(net D)(type fix))\n\
        (via \"Via[0-1]\" 9 -9 (net E))\n\
        (via \"Via[0-1]\" 10 -10 (net F)(type fix))\n\
        )\n)\n";

    #[test]
    fn only_type_fix_copper_survives() {
        let out = strip_session_copper(DSN);
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("7 -7  8 -8"), "{text}");
        assert!(text.contains("10 -10"), "{text}");
        assert!(!text.contains("1 -1  2 -2"), "{text}");
        assert!(!text.contains("3 -3  4 -4"), "{text}");
        assert!(!text.contains("5 -5  6 -6"), "{text}");
        assert!(!text.contains("9 -9"), "{text}");
        // The scope itself, and everything outside it, is intact.
        assert!(text.starts_with("(pcb x\n(wiring"));
        assert!(text.ends_with(")\n"));
    }

    #[test]
    fn a_board_without_a_wiring_scope_is_returned_unchanged() {
        let src = b"(pcb x\n(structure)\n)\n";
        assert_eq!(strip_session_copper(src), src.to_vec());
    }

    #[test]
    fn non_copper_children_of_the_wiring_scope_are_kept() {
        let src = b"(pcb x\n(wiring\n(wire (path F.Cu 1  0 0  1 1)(net A))\n(net_out X)\n)\n)\n";
        let text = String::from_utf8(strip_session_copper(src)).unwrap();
        assert!(text.contains("(net_out X)"), "{text}");
        assert!(!text.contains("(path F.Cu"), "{text}");
    }

    #[test]
    fn a_quoted_paren_inside_the_scope_does_not_end_a_child_early() {
        let src = b"(pcb x\n(wiring\n(via \"Via)(0-1\" 1 -1 (net A)(type fix))\n)\n)\n";
        let text = String::from_utf8(strip_session_copper(src)).unwrap();
        assert!(text.contains("\"Via)(0-1\""), "{text}");
    }

    #[test]
    fn the_keyword_scan_ignores_leading_whitespace_and_stops_at_the_first_atom() {
        assert_eq!(scope_keyword(b"( wire (path))"), b"wire");
        assert_eq!(scope_keyword(b"(via\"q\")"), b"via");
        assert_eq!(scope_keyword(b"(wire_keepout x)"), b"wire_keepout");
    }

    #[test]
    fn only_the_exact_fix_token_counts_as_system_fixed() {
        assert!(is_system_fixed(b"(wire (path)(type fix))"));
        assert!(is_system_fixed(b"(wire (path)(type  fix ))"));
        assert!(!is_system_fixed(b"(wire (path)(type fixed))"));
        assert!(!is_system_fixed(b"(wire (path)(type shove_fixed))"));
        assert!(!is_system_fixed(b"(wire (path)(type protect))"));
        assert!(!is_system_fixed(b"(wire (path)(net fix))"));
    }
}
