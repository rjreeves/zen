use crate::ast::*;
use crate::lexer::{SpannedToken, Token};

pub struct Parser {
    tokens: Vec<SpannedToken>,
    pos: usize,
    source: Vec<String>, 
}

impl Parser {
    pub fn new(tokens: Vec<SpannedToken>, src: &str) -> Self {
        Self {
            tokens,
            pos: 0,
            source: src.lines().map(|l| l.to_string()).collect(),
        }
    }

    fn get_source_line(&self, line: usize) -> String {
        if line == 0 || line > self.source.len() {
            return "".into();
        }
        self.source[line - 1].clone()
    }

    fn current(&self) -> &Token {
        &self.tokens.get(self.pos).map(|t| &t.token).unwrap()
    }

    fn match_token(&mut self, expected: &Token) -> bool {
        if self.current() == expected {
            self.advance();
            true
        } else {
            false
        }
    }

    fn consume_newlines(&mut self) {
        while matches!(self.current(), Token::Newline) {
            self.advance();
        }
    }

    pub fn parse_program(&mut self) -> Result<Program, String> {
        let mut requires = Vec::new();
        let mut statements = Vec::new();

        self.consume_newlines();

        // ✅ Handle requires block FIRST
        if matches!(self.current(), Token::Requires) {
            requires = self.parse_requires()?;
        }

        self.consume_newlines();

        // Then parse statements
        while !matches!(self.current(), Token::EOF) {
            self.consume_newlines();

            if matches!(self.current(), Token::EOF) {
                break;
            }

            statements.push(self.parse_statement()?);

            self.consume_newlines();
    }

    Ok(Program { requires, statements })
}

    fn precedence(token: &Token) -> u8 {
    match token {
        Token::OrOr        => 1,
        Token::AndAnd      => 2,
        Token::EqEq | Token::NotEq => 3,
        Token::Greater | Token::GreaterEq | Token::Less | Token::LessEq => 4,
        Token::Plus | Token::Minus => 5,
        Token::Star | Token::Slash => 6,
        _ => 0,
    }
}




    fn parse_requires(&mut self) -> Result<Vec<(String, String)>, String> {
        self.advance(); // requires
        self.expect(Token::LBrace)?;

        let mut perms = Vec::new();

        self.consume_newlines();

        while !matches!(self.current(), Token::RBrace) {
            self.consume_newlines();

            let left = self.expect_ident()?;
            self.expect(Token::Dot)?;
            let right = self.expect_ident()?;

            perms.push((left, right));

            self.consume_newlines();
        }

        self.expect(Token::RBrace)?;
        Ok(perms)
    }

    fn parse_statement(&mut self) -> Result<Stmt, String> {
        if let Token::Ident(name) = self.current().clone() {
            if self.peek_is_equals() {
                return self.parse_assignment();
            }
        }

        if self.is_pipeline_start() {
            return Ok(Stmt::Pipeline(self.parse_pipeline()?));
        }

        Ok(Stmt::Expr(self.parse_expr()?))
    }

    fn parse_assignment(&mut self) -> Result<Stmt, String> {
        let name = self.expect_ident()?;
        self.expect(Token::Equals)?;
        let expr = self.parse_expr()?;
        Ok(Stmt::Assignment { name, expr })
    }

    fn parse_pipeline(&mut self) -> Result<Pipeline, String> {
        let base = self.parse_expr()?;
        let mut stages = Vec::new();


    loop {
        self.consume_newlines();

        if matches!(self.current(), Token::Pipe) {
            self.advance();
            stages.push(self.parse_pipe_stage()?);
        } else {
            break;
        }
    };

        Ok(Pipeline { base, stages })
    }

    fn parse_pipe_stage(&mut self) -> Result<PipeStage, String> {
        match self.current() {
            Token::Where => self.parse_where(),
            Token::Select => self.parse_select(),
            Token::Ident(name) if name == "sort" => self.parse_sort(),
            Token::Ident(name) if name == "limit" => self.parse_limit(),
            Token::Ident(name) if name == "count" => {
                self.advance();
                Ok(PipeStage::Count)
            }
            Token::Ident(name) if name == "avg" => self.parse_avg(),
            Token::Ident(name) if name == "sum" => self.parse_sum(),
            Token::Ident(name) if name == "max" => self.parse_max(),
            Token::Ident(name) if name == "min" => self.parse_min(),
            Token::Ident(name) if name == "distinct" => self.parse_distinct(),
            _ => Ok(PipeStage::Call(self.parse_call()?)),
            
        }
    }

    fn parse_where(&mut self) -> Result<PipeStage, String> {
        self.advance(); // where

        let expr = self.parse_expression();

        Ok(PipeStage::Where { expr })
    }

    fn parse_select(&mut self) -> Result<PipeStage, String> {
        self.advance(); // select
        let mut fields = Vec::new();

        loop {
            fields.push(self.expect_ident()?);
            if matches!(self.current(), Token::Comma) {
                self.advance();
            } else {
                break;
            }
        }

        Ok(PipeStage::Select { fields })
    }

    fn parse_logical_or(&mut self) -> Result<Expr, String> {
    let mut expr = self.parse_logical_and()?;

    while let Token::Ident(op) = self.current().clone() {
        if op == "||" {
            self.advance();
            let right = self.parse_logical_and()?;
            expr = Expr::Binary {
                left: Box::new(expr),
                op: BinOp::Or,
                right: Box::new(right),
            };
        } else {
            break;
        }
    }

        Ok(expr)
    }

    fn parse_logical_and(&mut self) -> Result<Expr, String> {
    let mut expr = self.parse_comparison()?;

    while let Token::Ident(op) = self.current().clone() {
        if op == "&&" {
            self.advance();
            let right = self.parse_comparison()?;
            expr = Expr::Binary {
                left: Box::new(expr),
                op: BinOp::And,
                right: Box::new(right),
            };
        } else {
            break;
        }
    }
        Ok(expr)
    }


    fn parse_comparison(&mut self) -> Result<Expr, String> {
        let mut expr = self.parse_term()?;

        while let Some(op) = self.match_comparison_op() {
            let right = self.parse_term()?;
            expr = Expr::Binary {
                left: Box::new(expr),
                op,
                right: Box::new(right),
            };
        }

        Ok(expr)
    }


    fn parse_term(&mut self) -> Result<Expr, String> {
    let mut expr = self.parse_factor()?;

    while let Some(op) = self.match_term_op() {
        let right = self.parse_factor()?;
        expr = Expr::Binary {
            left: Box::new(expr),
            op,
            right: Box::new(right),
        };
    }

        Ok(expr)
    }

    fn parse_factor(&mut self) -> Result<Expr, String> {
    let mut expr = self.parse_primary()?;

    while let Some(op) = self.match_factor_op() {
        let right = self.parse_primary()?;
        expr = Expr::Binary {
            left: Box::new(expr),
            op,
            right: Box::new(right),
        };
    }

        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr, String> {
        match self.current().clone() {
            Token::Ident(name) => {
                self.advance();
                Ok(Expr::Ident(name))
            }
            Token::Number(_) | Token::String(_) => {
                Ok(Expr::Literal(self.parse_literal()?))
            }
            _ => Err(self.error("Invalid expression")),
        }
    }


    fn match_comparison_op(&mut self) -> Option<BinOp> {
    match self.current() {
        Token::Greater => {
            self.advance();
            Some(BinOp::Gt)
        }
        Token::Less => {
            self.advance();
            Some(BinOp::Lt)
        }
        Token::GreaterEq => {
            self.advance();
            Some(BinOp::Gte)
        }
        Token::LessEq => {
            self.advance();
            Some(BinOp::Lte)
        }
        Token::EqEq => {
            self.advance();
            Some(BinOp::Eq)
        }
        Token::NotEq => {
            self.advance();
            Some(BinOp::Neq)
        }
        _ => None,
    }
}

    fn match_term_op(&mut self) -> Option<BinOp> {
        match self.current() {
            Token::Plus => {
                self.advance();
                Some(BinOp::Add)
            }
            Token::Minus => {
                self.advance();
                Some(BinOp::Sub)
            }
            _ => None,
        }
    }


    fn match_factor_op(&mut self) -> Option<BinOp> {
        match self.current() {
            Token::Star => {
                self.advance();
                Some(BinOp::Mul)
            }
            Token::Slash => {
                self.advance();
                Some(BinOp::Div)
            }
            _ => None,
        }
    }



    fn parse_sort(&mut self) -> Result<PipeStage, String> {
    // consume 'sort'
    self.advance();

    let field = self.expect_ident()?;

    let mut descending = false;

    if let Token::Ident(dir) = self.current().clone() {
        if dir == "desc" {
            descending = true;
            self.advance();
        } else if dir == "asc" {
            self.advance();
        }
    }

    Ok(PipeStage::Sort { field, descending })
}

    fn parse_avg(&mut self) -> Result<PipeStage, String> {
        // consume 'avg'
        self.advance();

        let field = self.expect_ident()?;

        Ok(PipeStage::Avg { field })
    }
    fn parse_limit(&mut self) -> Result<PipeStage, String> {
    // consume 'limit'
    self.advance();

    match self.current().clone() {
        Token::Number(n) => {
            self.advance();
            if n < 0.0 {
                return Err(self.error("limit must be a positive number"));
            }

            Ok(PipeStage::Limit {
                count: n as usize,
            })
        }
        _ => Err(self.error("Expected number after 'limit'")),
    }
}

    fn parse_sum(&mut self) -> Result<PipeStage, String> {
    // consume 'sum'
        self.advance();

        let field = self.expect_ident()?;

        Ok(PipeStage::Sum { field })
    }

    fn parse_max(&mut self) -> Result<PipeStage, String> {
        self.advance(); // consume 'max'
        let field = self.expect_ident()?;
        Ok(PipeStage::Max { field })
    }

    fn parse_min(&mut self) -> Result<PipeStage, String> {
        self.advance(); // consume 'min'
        let field = self.expect_ident()?;
        Ok(PipeStage::Min { field })
    }
    fn parse_distinct(&mut self) -> Result<PipeStage, String> {
        self.advance(); // consume 'distinct'

        let field = self.expect_ident()?;

        Ok(PipeStage::Distinct { field })
    }





    fn parse_expr(&mut self) -> Result<Expr, String> {
        match self.current() {
            Token::Ident(_) => {
                // If next token is Dot → it's a function call
                if self.peek_is_dot() {
                    let call = self.parse_call()?;
                    Ok(Expr::Call(call))
                } else {
                    let name = self.expect_ident()?;
                    Ok(Expr::Ident(name))
                }
            }

            Token::String(_) | Token::Number(_) | Token::True | Token::False => {
                Ok(Expr::Literal(self.parse_literal()?))
            }

            _ => Err(self.error("Unexpected token in expression")),
        }
    }

    fn peek_is_dot(&self) -> bool {
        matches!(
            self.tokens.get(self.pos + 1).map(|t| &t.token),
            Some(Token::Dot)
        )
    }


    fn parse_call(&mut self) -> Result<FunctionCall, String> {
        let mut name = vec![self.expect_ident()?];

        while matches!(self.current(), Token::Dot) {
            self.advance();
            name.push(self.expect_ident()?);
        }

        let mut args = Vec::new();
        while self.is_expr_start() {
            args.push(self.parse_expr()?);
        }

        Ok(FunctionCall { name, args })
    }

    fn parse_literal(&mut self) -> Result<Literal, String> {
        match self.current().clone() {
            Token::String(s) => {
                self.advance();
                Ok(Literal::String(s))
            }
            Token::Number(n) => {
                self.advance();
                Ok(Literal::Number(n))
            }
            Token::True => {
                self.advance();
                Ok(Literal::Bool(true))
            }
            Token::False => {
                self.advance();
                Ok(Literal::Bool(false))
            }
            _ => Err(self.error("Expected literal")),
        }
    }

    fn parse_operator(&mut self) -> Result<Op, String> {
        let op = match self.current() {
            Token::Greater => Op::Gt,
            Token::Less => Op::Lt,
            Token::GreaterEq => Op::Gte,
            Token::LessEq => Op::Lte,
            Token::EqEq => Op::Eq,
            Token::NotEq => Op::Neq,
            _ => return Err(self.error("Expected comparison operator")),
        };
        self.advance();
        Ok(op)
    }

    fn expect_ident(&mut self) -> Result<String, String> {
        if let Token::Ident(name) = self.current().clone() {
            self.advance();
            Ok(name)
        } else {
            Err(self.error("Expected identifier"))
        }
    }

    fn expect(&mut self, expected: Token) -> Result<(), String> {
        if *self.current() == expected {
            self.advance();
            Ok(())
        } else {
            Err(self.error(&format!("Expected {:?}", expected)))
        }
    }

    fn peek_is_equals(&self) -> bool {
        matches!(
            self.tokens.get(self.pos + 1).map(|t| &t.token),
            Some(Token::Equals)
        )
    }

    fn is_pipeline_start(&self) -> bool {
        matches!(self.current(), Token::Ident(_) | Token::String(_) | Token::Number(_))
    }

    fn is_expr_start(&self) -> bool {
        matches!(self.current(), Token::Ident(_) | Token::String(_) | Token::Number(_) | Token::True | Token::False)
    }

    fn error(&self, msg: &str) -> String {
        let token = &self.tokens[self.pos];

        let line = token.line;
        let col = token.col;

        let source_line = self.get_source_line(line);

        format!(
            "Syntax error at line {}, column {}:\n{}\n{}^\n{}",
            line,
            col,
            source_line,
            " ".repeat(col.saturating_sub(1)),
            msg
        )
    }

    fn parse_expression(&mut self) -> Expr {
        self.parse_precedence(0)
    }

    fn parse_precedence(&mut self, min_prec: u8) -> Expr {
        let mut left = self.parse_prefix();

        while let Some(op) = self.peek_token() {
            let prec = self.precedence(op);
            if prec < min_prec {
                break;
            }

            let op_token = self.advance().token.clone();
            let right = self.parse_precedence(prec + 1);

            left = Expr::Binary {
                left: Box::new(left),
                op: BinOp,
                right: Box::new(right),
            };
        }

        left
    }

    fn parse_prefix(&mut self) -> Expr {
    let token = self.advance().token.clone();

    match token {
        Token::Number(n) => Expr::Number(n),
        Token::String(s) => Expr::String(s),
        Token::True => Expr::Bool(true),
        Token::False => Expr::Bool(false),

        Token::Ident(name) => Expr::Ident(name),

        Token::Minus => {
            let expr = self.parse_precedence(6);
            Expr::Unary {
                op: Token::Minus,
                expr: Box::new(expr),
            }
        }

        Token::LBrace => {
            let expr = self.parse_expression();
            self.consume(Token::RBrace);
            expr
        }

        _ => panic!("Unexpected token: {:?}", token),
    }
}

    fn peek_token(&self) -> Option<&Token> {
    self.tokens.get(self.pos).map(|t| &t.token)
}

    fn advance(&mut self) -> &SpannedToken {
    let tok = &self.tokens[self.pos];
    self.pos += 1;
    tok
}

    fn consume(&mut self, expected: Token) {
    let tok = self.advance();
    if tok.token != expected {
        panic!("Expected {:?}, got {:?}", expected, tok.token);
    }
}   



}