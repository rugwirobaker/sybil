//! SQL lexer (tokenizer).
//!
//! Transforms a SQL string into a stream of tokens consumed by the parser.

use thiserror::Error;

/// Errors produced during lexing.
#[derive(Debug, Error)]
pub enum LexError {
    #[error("invalid number: {0}")]
    InvalidNumber(String),

    #[error("unterminated string literal at position {position}")]
    UnterminatedString { position: usize },

    #[error("unexpected character: '{ch}' at position {position}")]
    UnexpectedChar { ch: char, position: usize },
}

/// A token produced by the lexer.
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // Keywords
    Select,
    From,
    Where,
    And,
    Or,
    Not,
    In,
    Between,
    Is,
    Null,
    Like,
    As,
    Order,
    By,
    Group,
    Limit,
    Offset,
    Asc,
    Desc,
    Show,
    Tables,
    Describe,
    Explain,
    Analyze,
    Buffers,
    True,
    False,

    // Literals
    Integer(i64),
    Float(f64),
    StringLiteral(String),

    // Identifiers
    Ident(String),

    // Operators
    Eq,
    Neq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    Plus,
    Minus,
    Star,
    Slash,

    // Punctuation
    LParen,
    RParen,
    Comma,
    Semicolon,
    Dot,

    // End
    Eof,
}

impl Token {
    /// Human-readable description of this token, for error messages.
    pub fn describe(&self) -> String {
        match self {
            Token::Select => "SELECT".into(),
            Token::From => "FROM".into(),
            Token::Where => "WHERE".into(),
            Token::And => "AND".into(),
            Token::Or => "OR".into(),
            Token::Not => "NOT".into(),
            Token::In => "IN".into(),
            Token::Between => "BETWEEN".into(),
            Token::Is => "IS".into(),
            Token::Null => "NULL".into(),
            Token::Like => "LIKE".into(),
            Token::As => "AS".into(),
            Token::Order => "ORDER".into(),
            Token::By => "BY".into(),
            Token::Group => "GROUP".into(),
            Token::Limit => "LIMIT".into(),
            Token::Offset => "OFFSET".into(),
            Token::Asc => "ASC".into(),
            Token::Desc => "DESC".into(),
            Token::Show => "SHOW".into(),
            Token::Tables => "TABLES".into(),
            Token::Describe => "DESCRIBE".into(),
            Token::Explain => "EXPLAIN".into(),
            Token::Analyze => "ANALYZE".into(),
            Token::Buffers => "BUFFERS".into(),
            Token::True => "TRUE".into(),
            Token::False => "FALSE".into(),
            Token::Integer(n) => format!("integer({n})"),
            Token::Float(n) => format!("float({n})"),
            Token::StringLiteral(s) => format!("string('{s}')"),
            Token::Ident(s) => format!("identifier({s})"),
            Token::Eq => "'='".into(),
            Token::Neq => "'!='".into(),
            Token::Lt => "'<'".into(),
            Token::Gt => "'>'".into(),
            Token::LtEq => "'<='".into(),
            Token::GtEq => "'>='".into(),
            Token::Plus => "'+'".into(),
            Token::Minus => "'-'".into(),
            Token::Star => "'*'".into(),
            Token::Slash => "'/'".into(),
            Token::LParen => "'('".into(),
            Token::RParen => "')'".into(),
            Token::Comma => "','".into(),
            Token::Semicolon => "';'".into(),
            Token::Dot => "'.'".into(),
            Token::Eof => "end of input".into(),
        }
    }
}

/// SQL lexer that produces tokens from an input string.
pub struct Lexer {
    input: Vec<char>,
    pos: usize,
}

impl Lexer {
    pub fn new(input: &str) -> Self {
        Self {
            input: input.chars().collect(),
            pos: 0,
        }
    }

    /// Current byte position in the input (for error reporting).
    pub fn position(&self) -> usize {
        self.pos
    }

    fn peek_char(&self) -> Option<char> {
        self.input.get(self.pos).copied()
    }

    fn advance_char(&mut self) -> Option<char> {
        let ch = self.input.get(self.pos).copied();
        if ch.is_some() {
            self.pos += 1;
        }
        ch
    }

    fn skip_whitespace(&mut self) {
        while let Some(ch) = self.peek_char() {
            if ch.is_ascii_whitespace() {
                self.advance_char();
            } else {
                break;
            }
        }
    }

    /// Produce the next token from the input.
    pub fn next_token(&mut self) -> Result<Token, LexError> {
        self.skip_whitespace();

        let Some(ch) = self.peek_char() else {
            return Ok(Token::Eof);
        };

        match ch {
            '(' => {
                self.advance_char();
                Ok(Token::LParen)
            }
            ')' => {
                self.advance_char();
                Ok(Token::RParen)
            }
            ',' => {
                self.advance_char();
                Ok(Token::Comma)
            }
            ';' => {
                self.advance_char();
                Ok(Token::Semicolon)
            }
            '.' => {
                self.advance_char();
                Ok(Token::Dot)
            }
            '+' => {
                self.advance_char();
                Ok(Token::Plus)
            }
            '-' => {
                self.advance_char();
                Ok(Token::Minus)
            }
            '*' => {
                self.advance_char();
                Ok(Token::Star)
            }
            '/' => {
                self.advance_char();
                Ok(Token::Slash)
            }
            '=' => {
                self.advance_char();
                Ok(Token::Eq)
            }
            '!' => {
                let pos = self.pos;
                self.advance_char();
                if self.peek_char() == Some('=') {
                    self.advance_char();
                    Ok(Token::Neq)
                } else {
                    Err(LexError::UnexpectedChar {
                        ch: '!',
                        position: pos,
                    })
                }
            }
            '<' => {
                self.advance_char();
                if self.peek_char() == Some('=') {
                    self.advance_char();
                    Ok(Token::LtEq)
                } else {
                    Ok(Token::Lt)
                }
            }
            '>' => {
                self.advance_char();
                if self.peek_char() == Some('=') {
                    self.advance_char();
                    Ok(Token::GtEq)
                } else {
                    Ok(Token::Gt)
                }
            }
            '\'' => self.lex_string(),
            c if c.is_ascii_digit() => self.lex_number(),
            c if c.is_ascii_alphabetic() || c == '_' => self.lex_ident_or_keyword(),
            other => {
                let pos = self.pos;
                self.advance_char();
                Err(LexError::UnexpectedChar {
                    ch: other,
                    position: pos,
                })
            }
        }
    }

    fn lex_string(&mut self) -> Result<Token, LexError> {
        let start = self.pos;
        self.advance_char(); // consume opening quote

        let mut value = String::new();
        loop {
            match self.advance_char() {
                None => {
                    return Err(LexError::UnterminatedString { position: start });
                }
                Some('\'') => {
                    // Check for escaped quote ('')
                    if self.peek_char() == Some('\'') {
                        self.advance_char();
                        value.push('\'');
                    } else {
                        return Ok(Token::StringLiteral(value));
                    }
                }
                Some(ch) => value.push(ch),
            }
        }
    }

    fn lex_number(&mut self) -> Result<Token, LexError> {
        let start = self.pos;
        let mut has_dot = false;

        while let Some(ch) = self.peek_char() {
            if ch.is_ascii_digit() {
                self.advance_char();
            } else if ch == '.' && !has_dot {
                has_dot = true;
                self.advance_char();
            } else {
                break;
            }
        }

        let text: String = self.input[start..self.pos].iter().collect();

        if has_dot {
            text.parse::<f64>()
                .map(Token::Float)
                .map_err(|_| LexError::InvalidNumber(text))
        } else {
            text.parse::<i64>()
                .map(Token::Integer)
                .map_err(|_| LexError::InvalidNumber(text))
        }
    }

    fn lex_ident_or_keyword(&mut self) -> Result<Token, LexError> {
        let start = self.pos;

        while let Some(ch) = self.peek_char() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                self.advance_char();
            } else {
                break;
            }
        }

        let text: String = self.input[start..self.pos].iter().collect();
        let lower = text.to_ascii_lowercase();

        let token = match lower.as_str() {
            "select" => Token::Select,
            "from" => Token::From,
            "where" => Token::Where,
            "and" => Token::And,
            "or" => Token::Or,
            "not" => Token::Not,
            "in" => Token::In,
            "between" => Token::Between,
            "is" => Token::Is,
            "null" => Token::Null,
            "like" => Token::Like,
            "as" => Token::As,
            "order" => Token::Order,
            "by" => Token::By,
            "group" => Token::Group,
            "limit" => Token::Limit,
            "offset" => Token::Offset,
            "asc" => Token::Asc,
            "desc" => Token::Desc,
            "show" => Token::Show,
            "tables" => Token::Tables,
            "describe" => Token::Describe,
            "explain" => Token::Explain,
            "analyze" => Token::Analyze,
            "buffers" => Token::Buffers,
            "true" => Token::True,
            "false" => Token::False,
            _ => Token::Ident(text),
        };

        Ok(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex_all(input: &str) -> Result<Vec<Token>, LexError> {
        let mut lexer = Lexer::new(input);
        let mut tokens = Vec::new();
        loop {
            let tok = lexer.next_token()?;
            if tok == Token::Eof {
                break;
            }
            tokens.push(tok);
        }
        Ok(tokens)
    }

    #[test]
    fn keywords() {
        let tokens = lex_all("SELECT FROM WHERE AND OR NOT").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Select,
                Token::From,
                Token::Where,
                Token::And,
                Token::Or,
                Token::Not,
            ]
        );
    }

    #[test]
    fn identifiers() {
        let tokens = lex_all("api_gateway my_table col1").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Ident("api_gateway".into()),
                Token::Ident("my_table".into()),
                Token::Ident("col1".into()),
            ]
        );
    }

    #[test]
    fn operators() {
        let tokens = lex_all("= != < > <= >= + - * /").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Eq,
                Token::Neq,
                Token::Lt,
                Token::Gt,
                Token::LtEq,
                Token::GtEq,
                Token::Plus,
                Token::Minus,
                Token::Star,
                Token::Slash,
            ]
        );
    }

    #[test]
    fn punctuation() {
        let tokens = lex_all("( ) , ; .").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::LParen,
                Token::RParen,
                Token::Comma,
                Token::Semicolon,
                Token::Dot,
            ]
        );
    }

    #[test]
    fn integers() {
        let tokens = lex_all("42 1705323600000").unwrap();
        assert_eq!(
            tokens,
            vec![Token::Integer(42), Token::Integer(1705323600000)]
        );
    }

    #[test]
    fn floats() {
        let tokens = lex_all("3.14 0.5").unwrap();
        assert_eq!(tokens, vec![Token::Float(3.14), Token::Float(0.5)]);
    }

    #[test]
    fn string_literal() {
        let tokens = lex_all("'hello' 'world'").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::StringLiteral("hello".into()),
                Token::StringLiteral("world".into()),
            ]
        );
    }

    #[test]
    fn string_escaped_quote() {
        let tokens = lex_all("'it''s'").unwrap();
        assert_eq!(tokens, vec![Token::StringLiteral("it's".into())]);
    }

    #[test]
    fn unterminated_string() {
        let result = lex_all("'unterminated");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            LexError::UnterminatedString { .. }
        ));
    }

    #[test]
    fn case_insensitive_keywords() {
        let tokens = lex_all("SeLeCt FrOm WhErE").unwrap();
        assert_eq!(tokens, vec![Token::Select, Token::From, Token::Where]);
    }

    #[test]
    fn bare_bang_is_error() {
        let result = lex_all("!");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            LexError::UnexpectedChar { ch: '!', .. }
        ));
    }

    #[test]
    fn unexpected_char() {
        let result = lex_all("@");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            LexError::UnexpectedChar { ch: '@', .. }
        ));
    }

    #[test]
    fn all_keywords() {
        let input = "SELECT FROM WHERE AND OR NOT IN BETWEEN IS NULL LIKE AS \
                      ORDER BY GROUP LIMIT OFFSET ASC DESC SHOW TABLES \
                      DESCRIBE EXPLAIN ANALYZE BUFFERS TRUE FALSE";
        let tokens = lex_all(input).unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Select,
                Token::From,
                Token::Where,
                Token::And,
                Token::Or,
                Token::Not,
                Token::In,
                Token::Between,
                Token::Is,
                Token::Null,
                Token::Like,
                Token::As,
                Token::Order,
                Token::By,
                Token::Group,
                Token::Limit,
                Token::Offset,
                Token::Asc,
                Token::Desc,
                Token::Show,
                Token::Tables,
                Token::Describe,
                Token::Explain,
                Token::Analyze,
                Token::Buffers,
                Token::True,
                Token::False,
            ]
        );
    }

    #[test]
    fn empty_input() {
        let tokens = lex_all("").unwrap();
        assert!(tokens.is_empty());
    }

    #[test]
    fn whitespace_only() {
        let tokens = lex_all("   \t\n  ").unwrap();
        assert!(tokens.is_empty());
    }
}
