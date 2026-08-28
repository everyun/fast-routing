//! Clearance-class name algebra: how DSN `(clearance … (type …))` strings become clearance
//! class names, indices and item-class bindings.
//!
//! Ported from the two upstream call sites that build clearance classes at import time:
//!
//! * `app.freerouting.io.specctra.parser.Structure.set_clearance_rule` (`Structure.java:617-707`)
//!   — PCB-level and `(layer L (rule …))`-level rules.
//! * `app.freerouting.io.specctra.parser.Network.add_clearance_rule` (`Network.java:633-683`) and
//!   `Network.get_clearance_class` (`Network.java:687-720`) — `(class …)`-level rules.
//!
//! Spec: `docs/upstream-study/dsn-board-semantics.md` §7.1-§7.8.
//!
//! Everything here is a pure function over strings (plus two thin helpers that append to a
//! [`ClearanceMatrix`]); the DSN importer owns the mutation of `BoardRules`.
//!
//! # The four rules that are easy to get wrong
//!
//! 1. **`"wire"` is an alias, never a class.** It resolves to clearance class 1 (`"default"`) at
//!    PCB level, and to the net class's own matrix class at net-class level. Both comparisons are
//!    case-**sensitive** `String.equals` (`Structure.java:675`, `Network.java:690`), even though
//!    `ClearanceMatrix.get_no` is case-insensitive.
//! 2. **`(type a-b)` and `(type a_b)` take different parse branches.** The lexer splits `(type …)`
//!    on `-` (`Rule.java:262`, `nextStringList(DsnFile.CLASS_CLEARANCE_SEPARATOR)`), so `a-b`
//!    arrives as a *two-element* list and `a_b` as a *one-element* list. `Structure` branches on
//!    the list length; see [`structure_class_pair`].
//! 3. **`a_b_c` is silently dropped.** `Structure` uses `split("_", 2)` so a 3-part name still
//!    yields 2 parts and is accepted as `("a", "b_c")`, but `Network` uses `split("_")` with no
//!    limit and requires exactly 2 parts, so `a_b_c` is skipped without a warning
//!    (`Network.java:668-671`).
//! 4. **The reserved item classes are appended in a fixed order:** `via`, `smd`, `pin`, `area`
//!    (`Structure.java:718-723`), so on a fresh board they take indices 2, 3, 4, 5. Downstream
//!    items are resolved by index, so the order is load-bearing.

use crate::clearance_matrix::ClearanceMatrix;
use crate::default_item_clearance_classes::ItemClass;

/// The magic clearance-class name that aliases the `"default"` class instead of creating one.
pub const WIRE_CLASS_NAME: &str = "wire";

/// Index that `"wire"` resolves to in PCB/layer-level rules: class 1, `"default"`
/// (`Structure.java:676`, `BoardRules.default_clearance_class()`).
pub const WIRE_CLEARANCE_CLASS_NO: usize = 1;

/// Separator between a net class name and an item class name in a derived clearance class name,
/// and the separator `(type …)` lists are split on.
///
/// `DsnFile.CLASS_CLEARANCE_SEPARATOR = '-'` (`DsnFile.java:21`).
pub const CLASS_CLEARANCE_SEPARATOR: char = '-';

/// The pseudo class-pair that sets `BoardRules.pin_edge_to_turn_dist` instead of a matrix cell.
///
/// Matched case-insensitively at `Structure.java:637`; `Network.add_clearance_rule` has **no**
/// such branch, so at net-class level the string falls through to the generic split.
pub const SMD_TO_TURN_GAP: &str = "smd_to_turn_gap";

/// The reserved item clearance class names, in the exact order they are appended by
/// `create_default_clearance_classes` (`Structure.java:718-723`, `Network.java:593-598`).
pub const RESERVED_ITEM_CLASS_NAMES: [&str; 4] = ["via", "smd", "pin", "area"];

/// Maps one of the four reserved names to the [`ItemClass`] it binds in
/// `NetClass.default_item_clearance_classes`.
///
/// The upstream `switch` is on the raw string, so the match is case-sensitive and any other name
/// gets a matrix class but no item-class binding (`Structure.java:725-737`,
/// `Network.java:713-718`).
pub fn reserved_item_class(name: &str) -> Option<ItemClass> {
    match name {
        "via" => Some(ItemClass::Via),
        "pin" => Some(ItemClass::Pin),
        "smd" => Some(ItemClass::Smd),
        "area" => Some(ItemClass::Area),
        _ => None,
    }
}

/// True if `name` is the `"wire"` alias. Case-sensitive, matching `"wire".equals(...)`.
pub fn is_wire_alias(name: &str) -> bool {
    name == WIRE_CLASS_NAME
}

/// True if `name` is the `smd_to_turn_gap` pseudo pair (case-insensitive, `Structure.java:637`).
pub fn is_smd_to_turn_gap(name: &str) -> bool {
    name.eq_ignore_ascii_case(SMD_TO_TURN_GAP)
}

/// True if any entry of the `(type …)` list starts with `"wire_"` or ends with `"_wire"`.
///
/// `Structure.contains_wire_clearance_pair` (`Structure.java:709-716`). When true, the importer
/// materializes the four reserved item clearance classes before applying the rule.
///
/// Note the consequence of quirk 2 in the module docs: the `-`-separated encoding `(type wire-via)`
/// arrives as `["wire", "via"]`, and neither element starts with `"wire_"` nor ends with `"_wire"`,
/// so it does **not** trigger the default classes — only the `(type wire_via)` encoding does.
/// Both comparisons are case-sensitive.
pub fn contains_wire_clearance_pair<S: AsRef<str>>(class_pairs: &[S]) -> bool {
    class_pairs
        .iter()
        .any(|p| p.as_ref().starts_with("wire_") || p.as_ref().ends_with("_wire"))
}

/// Builds the clearance class name for an item class inside a net class.
///
/// `Network.get_clearance_class` (`Network.java:687-692`): `"wire"` maps onto the net class's own
/// class name, every other item class name becomes `"<netClass>-<itemClass>"`. So a `(class SIGNAL
/// … (clearance … (type wire_via)))` yields the matrix classes `SIGNAL` and `SIGNAL-via`
/// (`dsn-board-semantics.md` §7.6).
pub fn net_class_item_clearance_class_name(net_class_name: &str, item_class_name: &str) -> String {
    if is_wire_alias(item_class_name) {
        net_class_name.to_string()
    } else {
        format!("{net_class_name}{CLASS_CLEARANCE_SEPARATOR}{item_class_name}")
    }
}

/// Builds the name of a per-net-class default `ViaInfo`: `"<padstack>-<netClass>"`.
///
/// Same separator, different construction site (`Network.java:338`, `dsn-board-semantics.md`
/// §7.4 step 10).
pub fn default_via_info_name(padstack_name: &str, net_class_name: &str) -> String {
    format!("{padstack_name}{CLASS_CLEARANCE_SEPARATOR}{net_class_name}")
}

/// Outcome of resolving one entry of a `(clearance v (type …))` list into a class pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassPairResolution {
    /// The entry was `smd_to_turn_gap`: set `BoardRules.pin_edge_to_turn_dist` to the clearance
    /// value and write no matrix cell (`Structure.java:637-641`).
    SmdToTurnGap,
    /// A resolved `(first, second)` clearance class name pair. The caller resolves each name to an
    /// index (honouring the `"wire"` alias) and writes both directions of the matrix.
    Pair(String, String),
    /// The entry could not be split into two names; upstream `continue`s past it, sometimes with a
    /// warning. Nothing is written.
    Skip,
}

/// Resolves one entry of a PCB-level or layer-level `(clearance v (type …))` list.
///
/// Faithful transcription of the body of `Structure.set_clearance_rule`'s loop
/// (`Structure.java:636-672`). `all_pairs` is the whole `(type …)` list (its **length** selects the
/// parse branch), `current` is the entry being processed, and `string_quote` is the active DSN
/// string quote character (`(string_quote …)`, default `"`).
///
/// Branches, in upstream order:
///
/// 1. `smd_to_turn_gap` (case-insensitive) ⇒ [`ClassPairResolution::SmdToTurnGap`].
/// 2. `all_pairs.len() == 2` — the `-`-separated `(type a-b)` form, **v2 only**
///    (`Structure.java:643-651`). The pair is taken from the *whole list*, ignoring `current`, so
///    the rule is applied twice (idempotently) as the loop visits both entries. Every `"` is
///    stripped from both names, and a leading `_` is stripped from the second name. Note the
///    upstream loop body targets `curr_pair[1]` on **both** iterations, so a name like `__b` loses
///    two leading underscores; that double strip is reproduced here.
/// 3. `current` starts with `string_quote` — a quoted first name such as `"My Class"_via`
///    (`Structure.java:653-663`). The leading quote is dropped, the remainder is split at the next
///    quote, and the tail must start with `_`, which is then dropped. Otherwise upstream warns and
///    skips.
/// 4. Otherwise `split("_", 2)`: only the **first** underscore separates, so `a_b_c` resolves to
///    `("a", "b_c")` and a name with no underscore at all is skipped
///    (`Structure.java:665-671`).
pub fn structure_class_pair<S: AsRef<str>>(
    all_pairs: &[S],
    current: &str,
    string_quote: &str,
) -> ClassPairResolution {
    if is_smd_to_turn_gap(current) {
        return ClassPairResolution::SmdToTurnGap;
    }

    if all_pairs.len() == 2 {
        let mut first = all_pairs[0].as_ref().to_string();
        let mut second = all_pairs[1].as_ref().to_string();
        // Upstream loops over both slots but only ever strips a leading '_' from the second one,
        // and does so once per iteration -- hence two strip attempts (Structure.java:646-651).
        for i in 0..2 {
            if i == 0 {
                first = first.replace('"', "");
            } else {
                second = second.replace('"', "");
            }
            if let Some(rest) = second.strip_prefix('_') {
                second = rest.to_string();
            }
        }
        return ClassPairResolution::Pair(first, second);
    }

    if !string_quote.is_empty() && current.starts_with(string_quote) {
        let rest = &current[string_quote.len()..];
        let parts = java_split_limited(rest, string_quote, 2);
        if parts.len() != 2 {
            return ClassPairResolution::Skip;
        }
        let Some(tail) = parts[1].strip_prefix('_') else {
            return ClassPairResolution::Skip;
        };
        return ClassPairResolution::Pair(parts[0].clone(), tail.to_string());
    }

    let parts = java_split_limited(current, "_", 2);
    if parts.len() != 2 {
        // Pairs with more than one underscore like `smd_via_same_net` are not implemented, and a
        // name with no underscore has nothing to split.
        return ClassPairResolution::Skip;
    }
    ClassPairResolution::Pair(parts[0].clone(), parts[1].clone())
}

/// Resolves one entry of a net-class-level `(clearance v (type …))` list.
///
/// `Network.add_clearance_rule` (`Network.java:666-671`) splits on **every** underscore and
/// requires exactly two parts, so:
///
/// * `a_b` ⇒ `Some(("a", "b"))`,
/// * `a_b_c` ⇒ `None` (silently dropped — the difference from [`structure_class_pair`]),
/// * `a` ⇒ `None`,
/// * `a_b_` ⇒ `Some(("a", "b"))`, because `String.split` discards *trailing* empty fields,
/// * `_b` ⇒ `Some(("", "b"))`, because leading empties are kept.
///
/// There is no `smd_to_turn_gap` branch, no `string_quote` handling and no `-`-pair branch here.
pub fn network_class_pair(current: &str) -> Option<(String, String)> {
    let parts = java_split_all(current, '_');
    if parts.len() != 2 {
        return None;
    }
    Some((parts[0].clone(), parts[1].clone()))
}

/// Appends `name` to the clearance matrix (if absent) and reports the index it ended up at
/// together with the item class it binds, if any.
///
/// `Structure.append_clearance_class` (`Structure.java:725-737`). The item-class binding is
/// returned rather than applied, so the caller decides which `NetClass` receives it — upstream
/// always uses the default net class here.
pub fn append_clearance_class(
    clearance_matrix: &mut ClearanceMatrix,
    name: &str,
) -> (usize, Option<ItemClass>) {
    clearance_matrix.append_class(name);
    // Upstream re-reads the index instead of trusting append_class, so an already-existing class
    // (append_class returned false) resolves to its existing index. get_no is case-insensitive.
    let index = clearance_matrix.get_no(name).unwrap_or(0);
    (index, reserved_item_class(name))
}

/// Appends the four reserved item clearance classes in upstream order and returns their indices
/// and item-class bindings.
///
/// `Structure.create_default_clearance_classes` (`Structure.java:718-723`). On a fresh
/// `ClearanceMatrix::default_instance` the result is `via = 2, smd = 3, pin = 4, area = 5`.
/// Re-running it is a no-op that returns the existing indices.
pub fn create_default_clearance_classes(
    clearance_matrix: &mut ClearanceMatrix,
) -> Vec<(usize, Option<ItemClass>)> {
    RESERVED_ITEM_CLASS_NAMES
        .iter()
        .map(|name| append_clearance_class(clearance_matrix, name))
        .collect()
}

/// `String.split(separator, limit)` for a positive limit, with a literal separator.
///
/// Java keeps trailing empty fields when the limit is positive, and returns the whole input as a
/// single element when the separator does not occur.
fn java_split_limited(input: &str, separator: &str, limit: usize) -> Vec<String> {
    debug_assert!(limit >= 1);
    if separator.is_empty() {
        // Java treats an empty regex as a zero-width match before every character, so
        // `"abc".split("", 2)` is `["", "abc"]`. Only reachable with a degenerate `(string_quote)`.
        if input.is_empty() {
            return vec![String::new()];
        }
        return vec![String::new(), input.to_string()];
    }
    let mut parts: Vec<String> = Vec::new();
    let mut rest = input;
    while parts.len() + 1 < limit {
        match rest.find(separator) {
            Some(pos) => {
                parts.push(rest[..pos].to_string());
                rest = &rest[pos + separator.len()..];
            }
            None => break,
        }
    }
    parts.push(rest.to_string());
    parts
}

/// `String.split(separator)` (limit 0) for a single-character literal separator.
///
/// Java discards **trailing** empty fields at limit 0, so `"a_"` yields `["a"]` and `"__"` yields
/// an empty array, while leading and interior empties are kept (`"_b"` ⇒ `["", "b"]`). An input
/// with no separator at all yields the input itself.
fn java_split_all(input: &str, separator: char) -> Vec<String> {
    let mut parts: Vec<String> = input.split(separator).map(|s| s.to_string()).collect();
    while parts.last().is_some_and(|s| s.is_empty()) {
        parts.pop();
    }
    if parts.is_empty() && !input.contains(separator) {
        // No match at all: Java returns a one-element array holding the whole (empty) input.
        parts.push(input.to_string());
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_split_limited_matches_java() {
        assert_eq!(java_split_limited("a_b", "_", 2), vec!["a", "b"]);
        assert_eq!(java_split_limited("a_b_c", "_", 2), vec!["a", "b_c"]);
        assert_eq!(java_split_limited("abc", "_", 2), vec!["abc"]);
        // positive limit keeps trailing empties
        assert_eq!(java_split_limited("a_", "_", 2), vec!["a", ""]);
        assert_eq!(java_split_limited("_b", "_", 2), vec!["", "b"]);
    }

    #[test]
    fn java_split_all_discards_trailing_empties() {
        assert_eq!(java_split_all("a_b", '_'), vec!["a", "b"]);
        assert_eq!(java_split_all("a_b_c", '_'), vec!["a", "b", "c"]);
        assert_eq!(java_split_all("a_", '_'), vec!["a"]);
        assert_eq!(java_split_all("_b", '_'), vec!["", "b"]);
        assert!(java_split_all("__", '_').is_empty());
        assert_eq!(java_split_all("abc", '_'), vec!["abc"]);
        assert_eq!(java_split_all("", '_'), vec![""]);
    }
}
