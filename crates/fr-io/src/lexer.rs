//! Specctra DSN / SES S-expression tokenizer.

use crate::keyword::Keyword;

/// A lexical token in a Specctra DSN file.
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    OpenParen,
    CloseParen,
    Keyword(Keyword),
    String(String),
    Number(f64),
}

/// Tokenizer for Specctra S-expressions.
pub struct DsnLexer<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> DsnLexer<'a> {
    pub fn new(input: &'a str) -> Self {
        DsnLexer { input, pos: 0 }
    }

    /// Fetches the next token from the stream.
    pub fn next_token(&mut self) -> Option<Token> {
        self.skip_whitespace_and_comments();
        if self.pos >= self.input.len() {
            return None;
        }

        let bytes = self.input.as_bytes();
        let ch = bytes[self.pos];

        if ch == b'(' {
            self.pos += 1;
            return Some(Token::OpenParen);
        }
        if ch == b')' {
            self.pos += 1;
            return Some(Token::CloseParen);
        }

        // Quoted string (double or single quote)
        if ch == b'"' || ch == b'\'' {
            let quote_char = ch;
            // Handle standalone quote token like `(string_quote ")`
            if self.pos + 1 < self.input.len() && bytes[self.pos + 1] == b')' {
                self.pos += 1;
                return Some(Token::String((quote_char as char).to_string()));
            }

            self.pos += 1;
            let start = self.pos;
            while self.pos < self.input.len() && bytes[self.pos] != quote_char {
                self.pos += 1;
            }
            let s = &self.input[start..self.pos];
            if self.pos < self.input.len() && bytes[self.pos] == quote_char {
                self.pos += 1;
            }
            return Some(Token::String(s.to_string()));
        }

        // Identifier, keyword, or number
        let start = self.pos;
        while self.pos < self.input.len() {
            let b = bytes[self.pos];
            if b.is_ascii_whitespace() || b == b'(' || b == b')' || b == b'"' || b == b'\'' || b == b'#' {
                break;
            }
            self.pos += 1;
        }

        let raw = &self.input[start..self.pos];
        if let Ok(num) = raw.parse::<f64>() {
            Some(Token::Number(num))
        } else if let Some(kw) = Keyword::parse(raw) {
            Some(Token::Keyword(kw))
        } else {
            Some(Token::String(raw.to_string()))
        }
    }

    fn skip_whitespace_and_comments(&mut self) {
        let bytes = self.input.as_bytes();
        while self.pos < self.input.len() {
            let b = bytes[self.pos];
            if b.is_ascii_whitespace() {
                self.pos += 1;
            } else if b == b'#' {
                // Line comment
                while self.pos < self.input.len() && bytes[self.pos] != b'\n' {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lex_s_expression() {
        let input = r#"
        (pcb "test_board"
            (parser
                (unit mm)
                (resolution mm 1000)
            )
        )
        "#;
        let mut lexer = DsnLexer::new(input);
        assert_eq!(lexer.next_token(), Some(Token::OpenParen));
        assert_eq!(lexer.next_token(), Some(Token::Keyword(Keyword::Pcb)));
        assert_eq!(lexer.next_token(), Some(Token::String("test_board".to_string())));
        assert_eq!(lexer.next_token(), Some(Token::OpenParen));
        assert_eq!(lexer.next_token(), Some(Token::Keyword(Keyword::Parser)));
        assert_eq!(lexer.next_token(), Some(Token::OpenParen));
        assert_eq!(lexer.next_token(), Some(Token::Keyword(Keyword::Unit)));
        assert_eq!(lexer.next_token(), Some(Token::String("mm".to_string())));
        assert_eq!(lexer.next_token(), Some(Token::CloseParen));
    }
}
