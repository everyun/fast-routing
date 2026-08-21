//! Quoting and formatting rules for legal identifier names in EDA/Specctra files.
//!
//! Ported from `app.freerouting.datastructures.IdentifierType`.

use std::io::{self, Write};

/// Rules for formatting and quoting identifiers that may contain reserved characters,
/// whitespace, leading digits, or non-ASCII bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentifierType {
    reserved_chars: Vec<String>,
    string_quote: String,
}

impl IdentifierType {
    /// Creates a new `IdentifierType` with specified reserved character strings and quote delimiter.
    pub fn new<I, S>(reserved_chars: I, string_quote: &str) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        IdentifierType {
            reserved_chars: reserved_chars.into_iter().map(Into::into).collect(),
            string_quote: string_quote.to_string(),
        }
    }

    /// Standard Specctra DSN identifier rules (quotes with `"` and treats spaces, parens, quotes as reserved).
    pub fn specctra_default() -> Self {
        Self::new([" ", "\t", "\n", "\r", "(", ")"], "\"")
    }

    /// Returns `true` if `s` contains no reserved characters.
    pub fn is_legal(&self, s: &str) -> bool {
        for rc in &self.reserved_chars {
            if s.contains(rc) {
                return false;
            }
        }
        true
    }

    /// Determines whether the string requires quotes.
    pub fn needs_quotes(&self, s: &str) -> bool {
        // Reserved character check
        for rc in &self.reserved_chars {
            if s.contains(rc) {
                return true;
            }
        }
        // Non-ASCII check
        if !s.is_ascii() {
            return true;
        }
        // Starts with a digit or negative digit (e.g. "-123", "4pin")
        let trimmed = s.trim_start();
        if let Some(first) = trimmed.chars().next() {
            if first.is_ascii_digit() {
                return true;
            }
            if first == '-' {
                if let Some(second) = trimmed.chars().nth(1) {
                    if second.is_ascii_digit() {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Encloses `s` in the configured string quotes.
    pub fn quote(&self, s: &str) -> String {
        format!("{}{}{}", self.string_quote, s, self.string_quote)
    }

    /// Formats an identifier according to quoting and reserved character rules.
    pub fn format_identifier(&self, mut name: &str) -> String {
        // Strip surrounding double quotes if present on both sides
        while name.len() >= 2 && name.starts_with('"') && name.ends_with('"') {
            name = &name[1..name.len() - 1];
        }

        // If the name contains the quote character internally, remove it
        let cleaned = if name.contains(&self.string_quote) {
            name.replace(&self.string_quote, "")
        } else {
            name.to_string()
        };

        if self.needs_quotes(&cleaned) {
            self.quote(&cleaned)
        } else {
            cleaned
        }
    }

    /// Formats and writes the identifier to the given `Write` destination.
    pub fn write_to<W: Write>(&self, name: &str, writer: &mut W) -> io::Result<()> {
        let formatted = self.format_identifier(name);
        writer.write_all(formatted.as_bytes())
    }
}

impl Default for IdentifierType {
    fn default() -> Self {
        Self::specctra_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plain_identifier() {
        let ident = IdentifierType::specctra_default();
        assert_eq!(ident.format_identifier("GND"), "GND");
        assert_eq!(ident.format_identifier("R1_pin1"), "R1_pin1");
    }

    #[test]
    fn test_spaces_and_parentheses_quoted() {
        let ident = IdentifierType::specctra_default();
        assert_eq!(ident.format_identifier("Net with space"), "\"Net with space\"");
        assert_eq!(ident.format_identifier("U1(VCC)"), "\"U1(VCC)\"");
    }

    #[test]
    fn test_leading_numbers_quoted() {
        let ident = IdentifierType::specctra_default();
        assert_eq!(ident.format_identifier("123Net"), "\"123Net\"");
        assert_eq!(ident.format_identifier("-45Net"), "\"-45Net\"");
    }

    #[test]
    fn test_non_ascii_quoted() {
        let ident = IdentifierType::specctra_default();
        assert_eq!(ident.format_identifier("µC_Pin"), "\"µC_Pin\"");
    }

    #[test]
    fn test_write_to() {
        let ident = IdentifierType::specctra_default();
        let mut buf = Vec::new();
        ident.write_to("Test Net", &mut buf).unwrap();
        assert_eq!(String::from_utf8(buf).unwrap(), "\"Test Net\"");
    }
}
