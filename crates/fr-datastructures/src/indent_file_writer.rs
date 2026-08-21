//! Formatter/writer for indented S-expression structured text files (Specctra DSN/SES).
//!
//! Ported from `app.freerouting.datastructures.IndentFileWriter`.

use std::io::{self, Write};

/// An indented file writer managing scope nesting, parenthesis pairs, and whitespace.
#[derive(Debug)]
pub struct IndentFileWriter<W: Write> {
    writer: W,
    indent_string: String,
    begin_scope: String,
    end_scope: String,
    current_indent_level: usize,
}

impl<W: Write> IndentFileWriter<W> {
    /// Creates a new `IndentFileWriter` wrapping the given writer with two-space indentation.
    pub fn new(writer: W) -> Self {
        Self::with_indent(writer, "  ")
    }

    /// Creates a new `IndentFileWriter` with a custom indentation string.
    pub fn with_indent(writer: W, indent: &str) -> Self {
        IndentFileWriter {
            writer,
            indent_string: indent.to_string(),
            begin_scope: "(".to_string(),
            end_scope: ")".to_string(),
            current_indent_level: 0,
        }
    }

    /// Begins a new scope with `(`, optionally placing it on a new indented line.
    pub fn start_scope(&mut self, new_line: bool) -> io::Result<()> {
        if new_line {
            self.new_line()?;
        }
        self.writer.write_all(self.begin_scope.as_bytes())?;
        self.current_indent_level += 1;
        Ok(())
    }

    /// Begins a new scope on a new indented line.
    pub fn start_scope_newline(&mut self) -> io::Result<()> {
        self.start_scope(true)
    }

    /// Closes the innermost scope with `)` after a newline.
    pub fn end_scope(&mut self) -> io::Result<()> {
        if self.current_indent_level > 0 {
            self.current_indent_level -= 1;
        }
        self.new_line()?;
        self.writer.write_all(self.end_scope.as_bytes())?;
        Ok(())
    }

    /// Emits a newline followed by the current indentation level's spaces.
    pub fn new_line(&mut self) -> io::Result<()> {
        self.writer.write_all(b"\n")?;
        for _ in 0..self.current_indent_level {
            self.writer.write_all(self.indent_string.as_bytes())?;
        }
        Ok(())
    }

    /// Writes a raw string slice to the underlying writer.
    pub fn write_str(&mut self, s: &str) -> io::Result<()> {
        self.writer.write_all(s.as_bytes())
    }

    /// Returns the current scope indentation level.
    pub fn indent_level(&self) -> usize {
        self.current_indent_level
    }

    /// Flushes the inner writer.
    pub fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }

    /// Consumes this wrapper, returning the inner writer.
    pub fn into_inner(self) -> W {
        self.writer
    }

    /// Returns a reference to the inner writer.
    pub fn get_ref(&self) -> &W {
        &self.writer
    }

    /// Returns a mutable reference to the inner writer.
    pub fn get_mut(&mut self) -> &mut W {
        &mut self.writer
    }
}

impl<W: Write> Write for IndentFileWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writer.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_indent_structure() {
        let mut buffer = Vec::new();
        {
            let mut writer = IndentFileWriter::new(&mut buffer);
            writer.start_scope(false).unwrap();
            writer.write_str("pcb test_board").unwrap();
            writer.start_scope_newline().unwrap();
            writer.write_str("rules").unwrap();
            writer.end_scope().unwrap();
            writer.end_scope().unwrap();
        }

        let output = String::from_utf8(buffer).unwrap();
        assert_eq!(output, "(pcb test_board\n  (rules\n  )\n)");
    }
}
