use std::iter::Peekable;
use std::str::Chars;
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // Keywords
    Requires,
    Where,
    Select,
    True,
    False,

    // Symbols
    LBrace,   // {
    RBrace,   // }
    Pipe,     // |
    Equals,   // =
    Dot,      // .
    Comma,    // ,

    // Operators
    Greater,
    Less,
    GreaterEq,
    LessEq,
    EqEq,
    NotEq,

    Plus,       // +
    Minus,      // -
    Star,       // *
    Slash,      // /

   // Gt,         // >
   // Lt,         // <
   // Gte,        // >=
   // Lte,        // <=
   // Neq,        // !=

    AndAnd,     // &&
    OrOr,       // ||




    // Literals
    Ident(String),
    String(String),
    Number(f64),
    Newline,
    EOF,
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

                '|' => {
                    self.advance();
                    tokens.push(self.token(Token::Pipe));
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
                        return Err(self.error("Unexpected '!'"));
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
                    self.advance();
                    tokens.push(self.make_token(Token::Slash));
                }

                _ => {
                    return Err(self.error(&format!("Unexpected character '{}'", ch)));
                }
            }
        }

        tokens.push(self.token(Token::EOF));
        Ok(tokens)
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
                        value.push(match escaped {
                            'n' => '\n',
                            't' => '\t',
                            '"' => '"',
                            '\\' => '\\',
                            other => other,
                        });
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

        if let Some(suffix) = self.peek() {
            match suffix {
                'k' | 'K' => {
                    self.advance();
                    multiplier = 1024.0;
                }
                'm' | 'M' => {
                    self.advance();
                    multiplier = 1024.0 * 1024.0;
                }
                'g' | 'G' => {
                    self.advance();
                    multiplier = 1024.0 * 1024.0 * 1024.0;
                }
                _ => {}
            }
            if matches!(suffix, 'k' | 'K' | 'm' | 'M' | 'g' | 'G') {
                // Optional trailing 'b'
                if self.peek() == Some('b') || self.peek() == Some('B') {
                    self.advance();
                }
            }
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
            "true" => Token::True,
            "false" => Token::False,
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
}