#![allow(dead_code)]

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // Keywords
    Requires,
    Where,
    Select,
    If,
    Else,
    Try,
    Catch,
    Finally,
    True,
    False,
    Null,

    // Symbols
    LBrace,   // {
    RBrace,   // }
    LBracket, // [
    RBracket, // ]
    Pipe,     // |
    Equals,   // =
    Dot,      // .
    Comma,    // ,
    Colon,    // :
    Dollar,   // $

    // Operators
    Greater,
    Less,
    GreaterEq,
    LessEq,
    EqEq,
    NotEq,

    Plus,      // +
    Minus,     // -
    Star,      // *
    Slash,     // /
    Backslash, // \

    // Gt,         // >
    // Lt,         // <
    // Gte,        // >=
    // Lte,        // <=
    // Neq,        // !=
    AndAnd, // &&
    OrOr,   // ||

    Bang, // !

    // Literals
    Ident(String),
    String(String),
    Number(f64),
    Newline,
    Eof,
}

#[derive(Debug, Clone)]
pub struct SpannedToken {
    pub token: Token,
    pub line: usize,
    pub col: usize,
}

pub struct Lexer {
    input: Vec<char>,
    pos: usize,
    line: usize,
    col: usize,
}

impl Lexer {
    pub fn new(input: &str) -> Self {
        Self {
            input: input.chars().collect(),
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    fn make_token(&self, token: Token) -> SpannedToken {
        SpannedToken {
            token,
            line: self.line,
            col: self.col,
        }
    }

    pub fn tokenize(mut self) -> Result<Vec<SpannedToken>, String> {
        let mut tokens = Vec::new();

        while let Some(ch) = self.peek() {
            match ch {
                ' ' | '\t' | '\r' => {
                    self.advance();
                }

                '#' => {
                    self.skip_comment();
                }

                '\n' => {
                    self.advance();
                    tokens.push(self.token(Token::Newline));
                    //        self.line += 1;
                    self.col = 0;
                }

                '{' => {
                    self.advance();
                    tokens.push(self.token(Token::LBrace));
                }

                '}' => {
                    self.advance();
                    tokens.push(self.token(Token::RBrace));
                }

                '[' => {
                    self.advance();
                    tokens.push(self.token(Token::LBracket));
                }

                ']' => {
                    self.advance();
                    tokens.push(self.token(Token::RBracket));
                }

                '|' => {
                    self.advance();
                    if self.peek() == Some('|') {
                        self.advance();
                        tokens.push(self.token(Token::OrOr));
                    } else {
                        tokens.push(self.token(Token::Pipe));
                    }
                }

                '&' => {
                    self.advance();
                    if self.peek() == Some('&') {
                        self.advance();
                        tokens.push(self.token(Token::AndAnd));
                    } else {
                        return Err(self.error("Unexpected '&'; use '&&' for logical and"));
                    }
                }

                '=' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        tokens.push(self.token(Token::EqEq));
                    } else {
                        tokens.push(self.token(Token::Equals));
                    }
                }

                '!' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        tokens.push(self.token(Token::NotEq));
                    } else {
                        tokens.push(self.token(Token::Bang));
                    }
                }

                '>' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        tokens.push(self.token(Token::GreaterEq));
                    } else {
                        tokens.push(self.token(Token::Greater));
                    }
                }

                '<' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        tokens.push(self.token(Token::LessEq));
                    } else {
                        tokens.push(self.token(Token::Less));
                    }
                }

                '.' => {
                    self.advance();
                    tokens.push(self.token(Token::Dot));
                }

                ',' => {
                    self.advance();
                    tokens.push(self.token(Token::Comma));
                }

                ':' => {
                    self.advance();
                    tokens.push(self.token(Token::Colon));
                }

                '$' => {
                    self.advance();
                    tokens.push(self.token(Token::Dollar));
                }

                '"' => {
                    tokens.push(self.read_string()?);
                }

                c if c.is_ascii_digit() => {
                    tokens.push(self.read_number()?);
                }

                c if is_ident_start(c) => {
                    tokens.push(self.read_ident_or_keyword());
                }

                '+' => {
                    self.advance();
                    tokens.push(self.make_token(Token::Plus));
                }

                '-' => {
                    self.advance();
                    tokens.push(self.make_token(Token::Minus));
                }

                '*' => {
                    let token = self.make_token(Token::Star);
                    self.advance();
                    tokens.push(token);
                }

                '/' => {
                    if self.peek_next() == Some('/') {
                        self.skip_comment();
                    } else {
                        self.advance();
                        tokens.push(self.make_token(Token::Slash));
                    }
                }

                '\\' => {
                    self.advance();
                    tokens.push(self.make_token(Token::Backslash));
                }

                _ => {
                    return Err(self.error(&format!("Unexpected character '{}'", ch)));
                }
            }
        }

        tokens.push(self.token(Token::Eof));
        Ok(tokens)
    }

    fn skip_comment(&mut self) {
        while let Some(ch) = self.peek() {
            if ch == '\n' {
                break;
            }
            self.advance();
        }
    }

    fn read_string(&mut self) -> Result<SpannedToken, String> {
        self.advance(); // opening "
        let start_col = self.col;
        let mut value = String::new();

        while let Some(ch) = self.peek() {
            match ch {
                '"' => {
                    self.advance();
                    return Ok(self.token(Token::String(value)));
                }
                '\\' => {
                    self.advance();
                    if let Some(escaped) = self.peek() {
                        self.advance();
                        match escaped {
                            'n' => value.push('\n'),
                            't' => value.push('\t'),
                            '"' => value.push('"'),
                            '\\' => value.push('\\'),
                            other => {
                                value.push('\\');
                                value.push(other);
                            }
                        }
                    }
                }
                _ => {
                    self.advance();
                    value.push(ch);
                }
            }
        }

        Err(self.error_at(start_col, "Unterminated string literal"))
    }

    fn read_number(&mut self) -> Result<SpannedToken, String> {
        let mut raw = String::new();

        while let Some(ch) = self.peek() {
            if ch.is_ascii_digit() || ch == '.' {
                self.advance();
                raw.push(ch);
            } else {
                break;
            }
        }

        let mut multiplier = 1.0;

        if matches!(self.peek(), Some('k' | 'K' | 'm' | 'M' | 'g' | 'G'))
            && matches!(self.peek_next(), Some('b' | 'B'))
        {
            let suffix = self.advance().unwrap();
            multiplier = match suffix {
                'k' | 'K' => 1024.0,
                'm' | 'M' => 1024.0 * 1024.0,
                'g' | 'G' => 1024.0 * 1024.0 * 1024.0,
                _ => 1.0,
            };
            self.advance();
        }

        let num: f64 = raw.parse().map_err(|_| self.error("Invalid number"))?;
        Ok(self.token(Token::Number(num * multiplier)))
    }

    fn read_ident_or_keyword(&mut self) -> SpannedToken {
        let mut value = String::new();

        while let Some(ch) = self.peek() {
            if is_ident_continue(ch) {
                self.advance();
                value.push(ch);
            } else {
                break;
            }
        }

        let token = match value.as_str() {
            "requires" => Token::Requires,
            "where" => Token::Where,
            "select" => Token::Select,
            "if" => Token::If,
            "else" => Token::Else,
            "try" => Token::Try,
            "catch" => Token::Catch,
            "finally" => Token::Finally,
            "true" => Token::True,
            "false" => Token::False,
            "null" => Token::Null,
            _ => Token::Ident(value),
        };

        self.token(token)
    }

    fn peek(&mut self) -> Option<char> {
        self.input.get(self.pos).copied()
    }

    fn peek_next(&self) -> Option<char> {
        self.input.get(self.pos + 1).copied()
    }

    fn advance(&mut self) -> Option<char> {
        let ch = self.input.get(self.pos).cloned();

        if let Some(c) = ch {
            self.pos += 1;

            if c == '\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
        }

        ch
    }
    fn token(&self, token: Token) -> SpannedToken {
        SpannedToken {
            token,
            line: self.line,
            col: self.col,
        }
    }

    fn error(&self, msg: &str) -> String {
        format!("{} at line {}, col {}", msg, self.line, self.col)
    }

    fn error_at(&self, col: usize, msg: &str) -> String {
        format!("{} at line {}, col {}", msg, self.line, col)
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lex_basic_script() {
        let src = r#"
requires {
  fs.read
}

files = fs.list "C:\logs"

files | where size > 1mb | select name, size
"#;

        let tokens = Lexer::new(src).tokenize().unwrap();
        assert!(!tokens.is_empty());
    }

    #[test]
    fn lexes_duration_suffixes_as_number_and_unit() {
        let tokens = Lexer::new("20ms 1m 2s").tokenize().unwrap();
        let kinds: Vec<Token> = tokens.into_iter().map(|token| token.token).collect();

        assert_eq!(
            kinds,
            vec![
                Token::Number(20.0),
                Token::Ident("ms".into()),
                Token::Number(1.0),
                Token::Ident("m".into()),
                Token::Number(2.0),
                Token::Ident("s".into()),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn lexes_size_suffixes_when_b_is_present() {
        let tokens = Lexer::new("1kb 1mb 1gb").tokenize().unwrap();
        let kinds: Vec<Token> = tokens.into_iter().map(|token| token.token).collect();

        assert_eq!(
            kinds,
            vec![
                Token::Number(1024.0),
                Token::Number(1024.0 * 1024.0),
                Token::Number(1024.0 * 1024.0 * 1024.0),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn lexes_square_brackets_for_list_literals() {
        let tokens = Lexer::new("[\"a\", \"b\"]").tokenize().unwrap();
        let kinds: Vec<Token> = tokens.into_iter().map(|token| token.token).collect();

        assert_eq!(
            kinds,
            vec![
                Token::LBracket,
                Token::String("a".into()),
                Token::Comma,
                Token::String("b".into()),
                Token::RBracket,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn keeps_unknown_backslash_escapes_in_strings() {
        let tokens = Lexer::new(r#""C:\Program Files\Tool\Tool.exe""#)
            .tokenize()
            .unwrap();
        let kinds: Vec<Token> = tokens.into_iter().map(|token| token.token).collect();

        assert_eq!(
            kinds,
            vec![
                Token::String(r"C:\Program Files\Tool\Tool.exe".into()),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn lexes_if_else_keywords() {
        let tokens = Lexer::new("if true { } else { }").tokenize().unwrap();
        let kinds: Vec<Token> = tokens.into_iter().map(|token| token.token).collect();

        assert_eq!(
            kinds,
            vec![
                Token::If,
                Token::True,
                Token::LBrace,
                Token::RBrace,
                Token::Else,
                Token::LBrace,
                Token::RBrace,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn lexes_logical_operators() {
        let tokens = Lexer::new("true && false || !false").tokenize().unwrap();
        let kinds: Vec<Token> = tokens.into_iter().map(|token| token.token).collect();

        assert_eq!(
            kinds,
            vec![
                Token::True,
                Token::AndAnd,
                Token::False,
                Token::OrOr,
                Token::Bang,
                Token::False,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn rejects_single_ampersand() {
        let err = Lexer::new("true & false").tokenize().unwrap_err();

        assert!(err.contains("use '&&'"));
    }
}
