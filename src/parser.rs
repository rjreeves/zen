#![allow(dead_code)]

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
        self.tokens.get(self.pos).map(|t| &t.token).unwrap()
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
        while !matches!(self.current(), Token::Eof) {
            self.consume_newlines();

            if matches!(self.current(), Token::Eof) {
                break;
            }

            statements.push(self.parse_statement()?);

            self.consume_newlines();
        }

        Ok(Program {
            requires,
            statements,
        })
    }

    fn precedence(token: &Token) -> u8 {
        match token {
            Token::OrOr => 1,
            Token::AndAnd => 2,
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
        if matches!(self.current(), Token::Colon) {
            return Err("Unexpected ':'; REPL commands are only available inside the REPL".into());
        }

        if matches!(self.current(), Token::If) {
            return self.parse_if_statement();
        }

        if matches!(self.current(), Token::Try) {
            return self.parse_try_statement();
        }

        if matches!(self.current(), Token::Ident(name) if name == "let") {
            return self.parse_let_assignment();
        }

        if let Token::Ident(_) = self.current().clone() {
            if self.peek_is_equals() {
                return self.parse_assignment();
            }
        }

        if self.is_pipeline_start() {
            let pipeline = self.parse_pipeline()?;
            return Ok(Stmt::Pipeline(pipeline));
        };

        Ok(Stmt::Expr(self.parse_expression()?))
    }

    fn parse_if_statement(&mut self) -> Result<Stmt, String> {
        self.advance(); // if
        let condition = self.parse_expression()?;
        let then_branch = self.parse_statement_block()?;

        self.consume_newlines();
        let else_branch = if matches!(self.current(), Token::Else) {
            self.advance();
            self.consume_newlines();

            if matches!(self.current(), Token::If) {
                vec![self.parse_if_statement()?]
            } else {
                self.parse_statement_block()?
            }
        } else {
            Vec::new()
        };

        Ok(Stmt::If {
            condition,
            then_branch,
            else_branch,
        })
    }

    fn parse_try_statement(&mut self) -> Result<Stmt, String> {
        self.advance(); // try
        let try_branch = self.parse_statement_block()?;

        self.consume_newlines();
        let (catch_name, catch_branch) = if matches!(self.current(), Token::Catch) {
            self.advance();
            let catch_name = match self.current().clone() {
                Token::Ident(name) => {
                    self.advance();
                    Some(name)
                }
                _ => None,
            };
            (catch_name, self.parse_statement_block()?)
        } else {
            (None, Vec::new())
        };

        self.consume_newlines();
        let finally_branch = if matches!(self.current(), Token::Finally) {
            self.advance();
            self.parse_statement_block()?
        } else {
            Vec::new()
        };

        if catch_branch.is_empty() && finally_branch.is_empty() {
            return Err(self.error("try requires catch or finally block"));
        }

        Ok(Stmt::Try {
            try_branch,
            catch_name,
            catch_branch,
            finally_branch,
        })
    }

    fn parse_statement_block(&mut self) -> Result<Vec<Stmt>, String> {
        self.expect(Token::LBrace)?;
        let mut statements = Vec::new();

        self.consume_newlines();
        while !matches!(self.current(), Token::RBrace) {
            if matches!(self.current(), Token::Eof) {
                return Err(self.error("Unterminated if block"));
            }

            statements.push(self.parse_statement()?);
            self.consume_newlines();
        }

        self.expect(Token::RBrace)?;
        Ok(statements)
    }

    fn parse_assignment(&mut self) -> Result<Stmt, String> {
        let name = self.expect_ident()?;
        self.expect(Token::Equals)?;
        let base = self.parse_expression()?;
        let value = if matches!(self.current(), Token::Pipe) {
            AssignmentValue::Pipeline(self.parse_pipeline_from_base(base)?)
        } else {
            AssignmentValue::Expr(base)
        };
        Ok(Stmt::Assignment { name, value })
    }

    fn parse_let_assignment(&mut self) -> Result<Stmt, String> {
        self.advance(); // let
        self.parse_assignment()
    }

    fn parse_pipeline(&mut self) -> Result<Pipeline, String> {
        let base = self.parse_expression()?;
        self.parse_pipeline_from_base(base)
    }

    fn parse_pipeline_from_base(&mut self, base: Expr) -> Result<Pipeline, String> {
        let mut stages = Vec::new();

        loop {
            self.consume_newlines();

            if matches!(self.current(), Token::Pipe) {
                self.advance();
                stages.push(self.parse_pipe_stage()?);
            } else {
                break;
            }
        }

        Ok(Pipeline { base, stages })
    }

    fn parse_pipe_stage(&mut self) -> Result<PipeStage, String> {
        match self.current() {
            Token::Where => self.parse_where(),
            Token::Select => self.parse_select(),
            Token::Ident(name) if name == "fields" => self.parse_fields(),
            Token::Ident(name) if name == "pick" => self.parse_pick(),
            Token::Ident(name) if name == "get" => self.parse_get(),
            Token::Ident(name) if name == "to-json" => {
                self.advance();
                Ok(PipeStage::ToJson)
            }
            Token::Ident(name) if name == "from-json" => {
                self.advance();
                Ok(PipeStage::FromJson)
            }
            Token::Ident(name) if name == "table" => {
                self.advance();
                Ok(PipeStage::Table)
            }
            Token::Ident(name) if name == "save" => self.parse_save(),
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

        let expr = self.parse_expression()?;

        Ok(PipeStage::Where { expr })
    }

    fn parse_select(&mut self) -> Result<PipeStage, String> {
        self.advance(); // select
        let fields = self.parse_field_list()?;

        Ok(PipeStage::Select { fields })
    }

    fn parse_fields(&mut self) -> Result<PipeStage, String> {
        self.advance(); // fields
        let fields = self.parse_field_list()?;

        Ok(PipeStage::Fields { fields })
    }

    fn parse_pick(&mut self) -> Result<PipeStage, String> {
        self.advance(); // pick
        let fields = self.parse_field_list()?;

        Ok(PipeStage::Fields { fields })
    }

    fn parse_get(&mut self) -> Result<PipeStage, String> {
        self.advance(); // get
        let field = self.expect_ident()?;

        Ok(PipeStage::Get { field })
    }

    fn parse_save(&mut self) -> Result<PipeStage, String> {
        self.advance(); // save
        let path = self.parse_path_arg()?;

        Ok(PipeStage::Save { path })
    }

    fn parse_field_list(&mut self) -> Result<Vec<String>, String> {
        let mut fields = Vec::new();

        loop {
            fields.push(self.expect_ident()?);
            if matches!(self.current(), Token::Comma) {
                self.advance();
            } else {
                break;
            }
        }

        Ok(fields)
    }

    fn parse_path_arg(&mut self) -> Result<String, String> {
        let mut out = match self.current().clone() {
            Token::String(value) | Token::Ident(value) => {
                self.advance();
                value
            }
            Token::Number(value) => {
                self.advance();
                value.to_string()
            }
            _ => return Err(self.error("Expected path after save")),
        };

        while matches!(self.current(), Token::Dot | Token::Slash | Token::Minus) {
            match self.current().clone() {
                Token::Dot => out.push('.'),
                Token::Slash => out.push('/'),
                Token::Minus => out.push('-'),
                _ => unreachable!(),
            }
            self.advance();

            match self.current().clone() {
                Token::Ident(value) | Token::String(value) => {
                    self.advance();
                    out.push_str(&value);
                }
                Token::Number(value) => {
                    self.advance();
                    out.push_str(&value.to_string());
                }
                _ => return Err(self.error("Expected path segment after separator")),
            }
        }

        Ok(out)
    }
    /*
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
    */

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
            Token::Dollar => {
                self.advance();
                Ok(Expr::Variable(self.expect_ident()?))
            }
            Token::LBracket | Token::LBrace => self.parse_expression(),
            Token::Number(_) | Token::String(_) => Ok(Expr::Literal(self.parse_literal()?)),
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

                Ok(PipeStage::Limit { count: n as usize })
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

            Token::Dollar => {
                self.advance();
                Ok(Expr::Variable(self.expect_ident()?))
            }

            Token::String(_) | Token::Number(_) | Token::True | Token::False | Token::Null => {
                Ok(Expr::Literal(self.parse_literal()?))
            }

            Token::LBracket | Token::LBrace => self.parse_expression(),

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

        let command_style = name.len() == 1
            && matches!(
                name[0].as_str(),
                "cd" | "pwd"
                    | "which"
                    | "help"
                    | "clear"
                    | "echo"
                    | "exec"
                    | "time"
                    | "measure"
                    | "benchmark"
                    | "sleep"
            );
        let mut args = Vec::new();
        while if command_style {
            self.is_command_arg_start_for(&name[0], args.is_empty())
        } else {
            self.is_call_arg_start()
        } {
            if command_style {
                args.push(self.parse_command_arg_for(&name[0], args.is_empty())?);
            } else {
                args.push(self.parse_expr()?);
            }
        }

        let config = if self.current_starts_call_config() {
            Some(self.parse_call_config()?)
        } else {
            None
        };

        Ok(FunctionCall { name, args, config })
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
            Token::Null => {
                self.advance();
                Ok(Literal::Null)
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
        let mut i = self.pos;

        while let Some(tok) = self.tokens.get(i) {
            match tok.token {
                Token::Pipe => return true,
                Token::Eof => break,
                _ => i += 1,
            }
        }

        false
    }

    fn is_expr_start(&self) -> bool {
        matches!(
            self.current(),
            Token::Ident(_)
                | Token::String(_)
                | Token::Number(_)
                | Token::True
                | Token::False
                | Token::Null
                | Token::LBracket
                | Token::Dollar
        )
    }

    fn is_call_arg_start(&self) -> bool {
        self.is_expr_start()
            || (self.current_starts_object_literal() && !self.current_starts_call_config())
    }

    fn is_command_arg_start(&self) -> bool {
        self.is_expr_start() || matches!(self.current(), Token::Minus)
    }

    fn is_command_arg_start_for(&self, command: &str, first_arg: bool) -> bool {
        self.is_command_arg_start()
            || (command == "exec" && first_arg && self.starts_exec_path_like_token())
    }

    fn parse_command_arg(&mut self) -> Result<Expr, String> {
        if matches!(self.current(), Token::Minus) {
            self.advance();
            if matches!(self.current(), Token::Minus) {
                self.advance();
                let name = self.expect_ident()?;
                return Ok(Expr::String(format!("--{}", name)));
            }

            let name = self.expect_ident()?;
            return Ok(Expr::String(format!("-{}", name)));
        }

        self.parse_expr()
    }

    fn parse_command_arg_for(&mut self, command: &str, first_arg: bool) -> Result<Expr, String> {
        if command == "exec" && first_arg && self.starts_exec_path_like_token() {
            return self.parse_exec_command();
        }

        self.parse_command_arg()
    }

    fn parse_exec_command(&mut self) -> Result<Expr, String> {
        match self.current().clone() {
            Token::String(value) => {
                self.advance();
                Ok(Expr::String(value))
            }
            Token::Ident(_) if !self.starts_exec_path_like_token() => self.parse_command_arg(),
            Token::Ident(_) | Token::Dot | Token::Slash | Token::Backslash => {
                Ok(Expr::String(self.parse_path_like_word()?))
            }
            _ => Err(self.error("Expected command name or executable path after exec")),
        }
    }

    fn starts_exec_path_like_token(&self) -> bool {
        match self.current() {
            Token::Dot | Token::Slash | Token::Backslash => true,
            Token::Ident(_) => matches!(
                self.tokens.get(self.pos + 1).map(|token| &token.token),
                Some(Token::Colon | Token::Dot | Token::Slash | Token::Backslash)
            ),
            _ => false,
        }
    }

    fn parse_path_like_word(&mut self) -> Result<String, String> {
        let mut out = String::new();
        let mut allow_segment = true;

        loop {
            match self.current().clone() {
                Token::Ident(value) if allow_segment => {
                    out.push_str(&value);
                    self.advance();
                    allow_segment = false;
                }
                Token::Number(value) if allow_segment => {
                    out.push_str(&value.to_string());
                    self.advance();
                    allow_segment = false;
                }
                Token::Dot | Token::Slash | Token::Backslash | Token::Colon => {
                    let ch = match self.current() {
                        Token::Dot => '.',
                        Token::Slash => '/',
                        Token::Backslash => '\\',
                        Token::Colon => ':',
                        _ => unreachable!(),
                    };
                    out.push(ch);
                    self.advance();
                    allow_segment = true;
                }
                _ => break,
            }
        }

        if out.is_empty() {
            return Err(self.error("Expected command name or executable path after exec"));
        }

        Ok(out)
    }

    fn parse_call_config(&mut self) -> Result<CallConfig, String> {
        self.expect(Token::LBrace)?;
        self.consume_newlines();

        let mut env = Vec::new();

        while !matches!(self.current(), Token::RBrace) {
            let section = self.expect_ident()?;
            self.expect(Token::Colon)?;
            self.consume_newlines();

            match section.as_str() {
                "env" => env.extend(self.parse_env_block()?),
                _ => return Err(self.error(&format!("Unknown exec config section '{}'", section))),
            }

            self.consume_newlines();
            if matches!(self.current(), Token::Comma) {
                self.advance();
                self.consume_newlines();
            }
        }

        self.expect(Token::RBrace)?;
        Ok(CallConfig { env })
    }

    fn parse_env_block(&mut self) -> Result<Vec<(String, Expr)>, String> {
        self.expect(Token::LBrace)?;
        self.consume_newlines();

        let mut env = Vec::new();

        while !matches!(self.current(), Token::RBrace) {
            let key = self.expect_ident()?;
            self.expect(Token::Colon)?;
            let value = self.parse_config_value()?;
            env.push((key, value));

            self.consume_newlines();
            if matches!(self.current(), Token::Comma) {
                self.advance();
                self.consume_newlines();
            }
        }

        self.expect(Token::RBrace)?;
        Ok(env)
    }

    fn parse_config_value(&mut self) -> Result<Expr, String> {
        if matches!(self.current(), Token::Dollar) {
            self.advance();
            return Ok(Expr::Variable(self.expect_ident()?));
        }

        self.parse_expr()
    }

    fn error(&self, msg: &str) -> String {
        let token = &self.tokens[self.pos];

        self.error_at(token.line, token.col, msg)
    }

    fn error_at(&self, line: usize, col: usize, msg: &str) -> String {
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

    fn parse_expression(&mut self) -> Result<Expr, String> {
        self.parse_precedence(0)
    }

    fn token_to_binop(token: &Token) -> Option<BinOp> {
        match token {
            Token::Plus => Some(BinOp::Add),
            Token::Minus => Some(BinOp::Sub),
            Token::Star => Some(BinOp::Mul),
            Token::Slash => Some(BinOp::Div),

            Token::Greater => Some(BinOp::Gt),
            Token::Less => Some(BinOp::Lt),
            Token::GreaterEq => Some(BinOp::Gte),
            Token::LessEq => Some(BinOp::Lte),

            Token::EqEq => Some(BinOp::Eq),
            Token::NotEq => Some(BinOp::Neq),

            Token::AndAnd => Some(BinOp::And),
            Token::OrOr => Some(BinOp::Or),

            _ => None,
        }
    }

    fn parse_precedence(&mut self, min_prec: u8) -> Result<Expr, String> {
        let mut left = self.parse_prefix()?;

        while let Some(op) = self.peek_token() {
            let prec = Self::precedence(op);
            if prec == 0 || prec < min_prec {
                break;
            }

            let op_token = self.advance().token.clone();
            let op = Self::token_to_binop(&op_token)
                .ok_or_else(|| self.error(&format!("Invalid operator: {:?}", op_token)))?;

            let right = self.parse_precedence(prec + 1)?;

            left = Expr::Binary {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }

        Ok(left)
    }

    fn parse_prefix(&mut self) -> Result<Expr, String> {
        let span = self.advance().clone();
        let token = span.token;

        match token {
            Token::Number(n) => Ok(Expr::Number(n)),
            Token::String(s) => Ok(Expr::String(s)),
            Token::True => Ok(Expr::Bool(true)),
            Token::False => Ok(Expr::Bool(false)),
            Token::Null => Ok(Expr::Literal(Literal::Null)),
            Token::LBracket => self.parse_list_literal(),
            Token::Dollar => Ok(Expr::Variable(self.expect_ident()?)),

            Token::Ident(name) => {
                if name == "exec" && self.starts_exec_path_like_token() {
                    let mut args = Vec::new();
                    while self.is_command_arg_start_for(&name, args.is_empty()) {
                        args.push(self.parse_command_arg_for(&name, args.is_empty())?);
                    }

                    let config = if self.current_starts_call_config() {
                        Some(self.parse_call_config()?)
                    } else {
                        None
                    };

                    return Ok(Expr::Call(FunctionCall {
                        name: vec![name],
                        args,
                        config,
                    }));
                }

                if matches!(self.current(), Token::Dot) {
                    let mut parts = vec![name];

                    while matches!(self.current(), Token::Dot) {
                        self.advance();
                        parts.push(self.expect_ident()?);
                    }

                    let mut args = Vec::new();
                    while self.is_call_arg_start() {
                        args.push(self.parse_expr()?);
                    }

                    let config = if self.current_starts_call_config() {
                        Some(self.parse_call_config()?)
                    } else {
                        None
                    };

                    Ok(Expr::Call(FunctionCall {
                        name: parts,
                        args,
                        config,
                    }))
                } else if matches!(
                    name.as_str(),
                    "cd" | "pwd"
                        | "which"
                        | "help"
                        | "clear"
                        | "exec"
                        | "echo"
                        | "time"
                        | "measure"
                        | "benchmark"
                        | "sleep"
                ) {
                    let mut args = Vec::new();
                    while self.is_command_arg_start_for(&name, args.is_empty()) {
                        args.push(self.parse_command_arg_for(&name, args.is_empty())?);
                    }

                    let config = if self.current_starts_call_config() {
                        Some(self.parse_call_config()?)
                    } else {
                        None
                    };

                    Ok(Expr::Call(FunctionCall {
                        name: vec![name],
                        args,
                        config,
                    }))
                } else {
                    Ok(Expr::Ident(name))
                }
            }

            Token::Minus => {
                let expr = self.parse_precedence(6)?;
                Ok(Expr::Unary {
                    op: Token::Minus,
                    expr: Box::new(expr),
                })
            }

            Token::Bang => {
                let expr = self.parse_precedence(6)?;
                Ok(Expr::Unary {
                    op: Token::Bang,
                    expr: Box::new(expr),
                })
            }

            Token::LBrace => self.parse_brace_expr(),

            _ => Err(self.error_at(
                span.line,
                span.col,
                &format!("Unexpected token: {:?}", token),
            )),
        }
    }

    fn parse_list_literal(&mut self) -> Result<Expr, String> {
        let mut items = Vec::new();

        self.consume_newlines();

        while !matches!(self.current(), Token::RBracket) {
            items.push(self.parse_expression()?);
            self.consume_newlines();

            if matches!(self.current(), Token::Comma) {
                self.advance();
                self.consume_newlines();
            } else {
                break;
            }
        }

        self.consume(Token::RBracket)?;
        Ok(Expr::List(items))
    }

    fn parse_brace_expr(&mut self) -> Result<Expr, String> {
        self.consume_newlines();

        if matches!(self.current(), Token::RBrace) {
            self.advance();
            return Ok(Expr::Object(Vec::new()));
        }

        if self.current_starts_object_entry() {
            return self.parse_object_literal();
        }

        let expr = self.parse_expression()?;
        self.consume(Token::RBrace)?;
        Ok(expr)
    }

    fn current_starts_object_entry(&self) -> bool {
        Self::token_can_start_object_key(self.current())
            && matches!(
                self.tokens.get(self.pos + 1).map(|token| &token.token),
                Some(Token::Colon)
            )
    }

    fn current_starts_object_literal(&self) -> bool {
        if !matches!(self.current(), Token::LBrace) {
            return false;
        }

        let mut i = self.pos + 1;
        while matches!(
            self.tokens.get(i).map(|token| &token.token),
            Some(Token::Newline)
        ) {
            i += 1;
        }

        matches!(
            self.tokens.get(i).map(|token| &token.token),
            Some(Token::RBrace)
        ) || (self
            .tokens
            .get(i)
            .is_some_and(|token| Self::token_can_start_object_key(&token.token))
            && matches!(
                self.tokens.get(i + 1).map(|token| &token.token),
                Some(Token::Colon)
            ))
    }

    fn token_can_start_object_key(token: &Token) -> bool {
        matches!(
            token,
            Token::Ident(_)
                | Token::String(_)
                | Token::If
                | Token::Try
                | Token::Catch
                | Token::Finally
        )
    }

    fn current_starts_call_config(&self) -> bool {
        if !matches!(self.current(), Token::LBrace) {
            return false;
        }

        let mut i = self.pos + 1;
        while matches!(
            self.tokens.get(i).map(|token| &token.token),
            Some(Token::Newline)
        ) {
            i += 1;
        }

        matches!(
            (
                self.tokens.get(i).map(|token| &token.token),
                self.tokens.get(i + 1).map(|token| &token.token),
            ),
            (Some(Token::Ident(name)), Some(Token::Colon)) if name == "env"
        )
    }

    fn parse_object_literal(&mut self) -> Result<Expr, String> {
        let mut entries = Vec::new();

        loop {
            self.consume_newlines();

            if matches!(self.current(), Token::RBrace) {
                self.advance();
                break;
            }

            let key = self.parse_object_key()?;
            self.consume(Token::Colon)?;
            let value = self.parse_expression()?;
            entries.push((key, value));

            self.consume_newlines();
            if matches!(self.current(), Token::Comma) {
                self.advance();
                continue;
            }

            self.consume(Token::RBrace)?;
            break;
        }

        Ok(Expr::Object(entries))
    }

    fn parse_object_key(&mut self) -> Result<String, String> {
        match self.current().clone() {
            Token::Ident(key) => {
                self.advance();
                Ok(key)
            }
            Token::String(key) => {
                self.advance();
                Ok(key)
            }
            Token::If => {
                self.advance();
                Ok("if".into())
            }
            Token::Try => {
                self.advance();
                Ok("try".into())
            }
            Token::Catch => {
                self.advance();
                Ok("catch".into())
            }
            Token::Finally => {
                self.advance();
                Ok("finally".into())
            }
            _ => Err(self.error("Expected object key")),
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

    fn consume(&mut self, expected: Token) -> Result<(), String> {
        let span = self.advance().clone();
        let actual = span.token;
        if actual != expected {
            return Err(self.error_at(
                span.line,
                span.col,
                &format!("Expected {:?}, got {:?}", expected, actual),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;

    fn parse(src: &str) -> Program {
        let tokens = Lexer::new(src).tokenize().unwrap();
        let mut parser = Parser::new(tokens, src);
        parser.parse_program().unwrap()
    }

    fn parse_err(src: &str) -> String {
        let tokens = Lexer::new(src).tokenize().unwrap();
        let mut parser = Parser::new(tokens, src);
        match parser.parse_program() {
            Ok(_) => panic!("Expected parse error"),
            Err(err) => err,
        }
    }

    fn parse_exec_args(src: &str) -> Vec<String> {
        let program = parse(src);
        match program.statements.as_slice() {
            [Stmt::Expr(Expr::Call(call))] if call.name == vec!["exec".to_string()] => call
                .args
                .iter()
                .map(|arg| match arg {
                    Expr::String(value) => value.clone(),
                    Expr::Ident(value) => value.clone(),
                    Expr::Literal(Literal::String(value)) => value.clone(),
                    Expr::Literal(Literal::Number(value)) => value.to_string(),
                    other => panic!("Unexpected exec arg expression: {:?}", expr_name(other)),
                })
                .collect(),
            _ => panic!("Expected exec call"),
        }
    }

    fn expr_name(expr: &Expr) -> &'static str {
        match expr {
            Expr::Ident(_) => "ident",
            Expr::Variable(_) => "variable",
            Expr::Number(_) => "number",
            Expr::String(_) => "string",
            Expr::Bool(_) => "bool",
            Expr::Literal(_) => "literal",
            Expr::List(_) => "list",
            Expr::Object(_) => "object",
            Expr::Call(_) => "call",
            Expr::Binary { .. } => "binary",
            Expr::Unary { .. } => "unary",
        }
    }

    #[test]
    fn bare_slash_reports_syntax_error() {
        let err = parse_err("/\n");

        assert!(err.contains("Syntax error"));
        assert!(err.contains("Unexpected token: Slash"));
    }

    #[test]
    fn parses_dotted_time_call_as_expression() {
        let program = parse("time.local\n");

        match program.statements.as_slice() {
            [Stmt::Expr(Expr::Call(call))] => {
                assert_eq!(call.name, vec!["time".to_string(), "local".to_string()]);
                assert!(call.args.is_empty());
            }
            _ => panic!("Expected dotted time expression"),
        }
    }

    #[test]
    fn parses_dotted_call_with_env_config() {
        let program = parse(
            "pg.query \"postgres://postgres@localhost:5432/postgres\" \"select 1\" { env: { PGPASSWORD: $pass } }\n",
        );

        match program.statements.as_slice() {
            [Stmt::Expr(Expr::Call(call))] => {
                assert_eq!(call.name, vec!["pg".to_string(), "query".to_string()]);
                assert_eq!(call.args.len(), 2);

                let config = call.config.as_ref().expect("Expected call config");
                assert_eq!(config.env.len(), 1);
                assert_eq!(config.env[0].0, "PGPASSWORD");
                match &config.env[0].1 {
                    Expr::Variable(name) => assert_eq!(name, "pass"),
                    _ => panic!("Expected PGPASSWORD to reference pass variable"),
                }
            }
            _ => panic!("Expected configured pg.query expression"),
        }
    }

    #[test]
    fn parses_dollar_variable_as_command_argument() {
        let program = parse("echo $pass\n");

        match program.statements.as_slice() {
            [Stmt::Expr(Expr::Call(call))] => {
                assert_eq!(call.name, vec!["echo".to_string()]);
                assert_eq!(call.args.len(), 1);
                match &call.args[0] {
                    Expr::Variable(name) => assert_eq!(name, "pass"),
                    _ => panic!("Expected echo argument to reference pass variable"),
                }
            }
            _ => panic!("Expected echo expression"),
        }
    }

    #[test]
    fn parses_dotted_time_call_in_pipeline_stage() {
        let program = parse("\"2026-05-14T12:34:56Z\" | time.local.format \"%Y\"\n");

        match program.statements.as_slice() {
            [Stmt::Pipeline(pipeline)] => match pipeline.stages.as_slice() {
                [PipeStage::Call(call)] => {
                    assert_eq!(
                        call.name,
                        vec![
                            "time".to_string(),
                            "local".to_string(),
                            "format".to_string()
                        ]
                    );
                    assert_eq!(call.args.len(), 1);
                }
                _ => panic!("Expected dotted time pipeline stage"),
            },
            _ => panic!("Expected pipeline"),
        }
    }

    #[test]
    fn parses_list_literal_assignment() {
        let program = parse("let parts = [\"a\", \"b\", \"c\"]\n");

        match program.statements.as_slice() {
            [Stmt::Assignment { name, value }] => {
                assert_eq!(name, "parts");
                match value {
                    AssignmentValue::Expr(Expr::List(items)) => assert_eq!(items.len(), 3),
                    _ => panic!("Expected list literal"),
                }
            }
            _ => panic!("Expected assignment"),
        }
    }

    #[test]
    fn parses_pipeline_assignment() {
        let program = parse("let t = time.now | time.format \"%I:%M:%S %p\"\n");

        match program.statements.as_slice() {
            [Stmt::Assignment { name, value }] => {
                assert_eq!(name, "t");
                match value {
                    AssignmentValue::Pipeline(pipeline) => {
                        assert!(
                            matches!(&pipeline.base, Expr::Call(call) if call.name == vec!["time".to_string(), "now".to_string()])
                        );
                        assert_eq!(pipeline.stages.len(), 1);
                    }
                    _ => panic!("Expected pipeline assignment"),
                }
            }
            _ => panic!("Expected assignment"),
        }
    }

    #[test]
    fn parses_object_literal_assignment() {
        let program = parse(
            "let user = { name: \"zen\", count: 3, active: true, tags: [\"cli\"], \"display-name\": \"Zen\", missing: null }\n",
        );

        match program.statements.as_slice() {
            [Stmt::Assignment { name, value }] => {
                assert_eq!(name, "user");
                match value {
                    AssignmentValue::Expr(Expr::Object(entries)) => {
                        assert_eq!(entries.len(), 6);
                        assert_eq!(entries[0].0, "name");
                        assert_eq!(entries[4].0, "display-name");
                    }
                    _ => panic!("Expected object literal"),
                }
            }
            _ => panic!("Expected assignment"),
        }
    }

    #[test]
    fn parses_if_else_statement() {
        let program = parse(
            "if ready == true {\n  let state = \"ok\"\n} else {\n  let state = \"missing\"\n}\n",
        );

        match program.statements.as_slice() {
            [Stmt::If {
                then_branch,
                else_branch,
                ..
            }] => {
                assert_eq!(then_branch.len(), 1);
                assert_eq!(else_branch.len(), 1);
            }
            _ => panic!("Expected if statement"),
        }
    }

    #[test]
    fn parses_try_catch_finally_statement() {
        let program = parse(
            "try {\n  missing.command\n} catch error {\n  let handled = error\n} finally {\n  let cleaned = true\n}\n",
        );

        match program.statements.as_slice() {
            [Stmt::Try {
                try_branch,
                catch_name,
                catch_branch,
                finally_branch,
            }] => {
                assert_eq!(try_branch.len(), 1);
                assert_eq!(catch_name.as_deref(), Some("error"));
                assert_eq!(catch_branch.len(), 1);
                assert_eq!(finally_branch.len(), 1);
            }
            _ => panic!("Expected try statement"),
        }
    }

    #[test]
    fn parses_if_after_dotted_call_without_treating_block_as_config() {
        let program =
            parse("if secrets.exists \"dropbox.refresh_token\" {\n  let state = \"saved\"\n}\n");

        match program.statements.as_slice() {
            [Stmt::If { condition, .. }] => match condition {
                Expr::Call(call) => {
                    assert_eq!(call.name, vec!["secrets".to_string(), "exists".to_string()]);
                    assert_eq!(call.args.len(), 1);
                    assert!(call.config.is_none());
                }
                _ => panic!("Expected call condition"),
            },
            _ => panic!("Expected if statement"),
        }
    }

    #[test]
    fn parses_multiline_nested_object_literal() {
        let program = parse("let user = {\n  name: \"zen\",\n  meta: {\n    ok: true,\n  },\n}\n");

        match program.statements.as_slice() {
            [Stmt::Assignment {
                value: AssignmentValue::Expr(Expr::Object(entries)),
                ..
            }] => {
                assert_eq!(entries.len(), 2);
                match &entries[1].1 {
                    Expr::Object(nested) => assert_eq!(nested.len(), 1),
                    _ => panic!("Expected nested object literal"),
                }
            }
            _ => panic!("Expected object assignment"),
        }
    }
    #[test]
    fn parses_list_literal_as_dotted_call_argument() {
        let program = parse("string.join [\"a\", \"b\", \"c\"] \",\"\n");

        match program.statements.as_slice() {
            [Stmt::Expr(Expr::Call(call))] => {
                assert_eq!(call.name, vec!["string".to_string(), "join".to_string()]);
                assert_eq!(call.args.len(), 2);
                match &call.args[0] {
                    Expr::List(items) => assert_eq!(items.len(), 3),
                    _ => panic!("Expected list argument"),
                }
            }
            _ => panic!("Expected string.join expression"),
        }
    }

    #[test]
    fn parses_measure_command_style_call() {
        let program = parse("measure time.local.format \"%Y\"\n");

        match program.statements.as_slice() {
            [Stmt::Expr(Expr::Call(call))] => {
                assert_eq!(call.name, vec!["measure".to_string()]);
                assert_eq!(call.args.len(), 1);
                match &call.args[0] {
                    Expr::Call(inner) => {
                        assert_eq!(
                            inner.name,
                            vec![
                                "time".to_string(),
                                "local".to_string(),
                                "format".to_string()
                            ]
                        );
                        assert_eq!(inner.args.len(), 1);
                    }
                    _ => panic!("Expected measured call argument"),
                }
            }
            _ => panic!("Expected measure expression"),
        }
    }

    #[test]
    fn parses_benchmark_command_style_call() {
        let program = parse("benchmark 10 sleep 20ms | select runs, min_ms\n");

        match program.statements.as_slice() {
            [Stmt::Pipeline(pipeline)] => {
                match &pipeline.base {
                    Expr::Call(call) => {
                        assert_eq!(call.name, vec!["benchmark".to_string()]);
                        assert_eq!(call.args.len(), 4);
                    }
                    _ => panic!("Expected benchmark call"),
                }
                assert!(matches!(
                    pipeline.stages.as_slice(),
                    [PipeStage::Select { .. }]
                ));
            }
            _ => panic!("Expected benchmark pipeline"),
        }
    }

    #[test]
    fn parses_fields_pipeline_stage() {
        let program = parse("workspace.files | fields name, path\n");

        match program.statements.as_slice() {
            [Stmt::Pipeline(pipeline)] => {
                assert!(matches!(
                    pipeline.stages.as_slice(),
                    [PipeStage::Fields { fields }]
                        if fields == &vec!["name".to_string(), "path".to_string()]
                ));
            }
            _ => panic!("Expected fields pipeline"),
        }
    }

    #[test]
    fn parses_data_pipeline_stages() {
        let program = parse(
            "exec tool | pick stdout, exitcode, success | where success == true | get stdout | to-json | save result.json\n",
        );

        match program.statements.as_slice() {
            [Stmt::Pipeline(pipeline)] => {
                assert!(matches!(
                    &pipeline.stages[0],
                    PipeStage::Fields { fields }
                        if fields == &vec![
                            "stdout".to_string(),
                            "exitcode".to_string(),
                            "success".to_string()
                        ]
                ));
                assert!(matches!(&pipeline.stages[1], PipeStage::Where { .. }));
                assert!(matches!(
                    &pipeline.stages[2],
                    PipeStage::Get { field } if field == "stdout"
                ));
                assert!(matches!(&pipeline.stages[3], PipeStage::ToJson));
                assert!(matches!(
                    &pipeline.stages[4],
                    PipeStage::Save { path } if path == "result.json"
                ));
            }
            _ => panic!("Expected data pipeline"),
        }
    }

    #[test]
    fn parses_exec_relative_executable_path() {
        let args = parse_exec_args("exec ./binn/firewisemail/FirewiseMail.App.exe\n");

        assert_eq!(args, vec!["./binn/firewisemail/FirewiseMail.App.exe"]);
    }

    #[test]
    fn parses_exec_windows_relative_executable_path() {
        let args = parse_exec_args(r"exec .\binn\firewisemail\FirewiseMail.App.exe");

        assert_eq!(args, vec![r".\binn\firewisemail\FirewiseMail.App.exe"]);
    }

    #[test]
    fn parses_exec_quoted_windows_executable_path() {
        let args = parse_exec_args(r#"exec "C:\Program Files\FirewiseMail\FirewiseMail.App.exe""#);

        assert_eq!(
            args,
            vec![r"C:\Program Files\FirewiseMail\FirewiseMail.App.exe"]
        );
    }

    #[test]
    fn parses_exec_path_with_timeout_option() {
        let args = parse_exec_args("exec ./tools/my-tool.exe --version timeout 10s\n");

        assert_eq!(
            args,
            vec!["./tools/my-tool.exe", "--version", "timeout", "10", "s"]
        );
    }

    #[test]
    fn parses_exec_path_with_workdir_option() {
        let args = parse_exec_args("exec ./tools/my-tool.exe workdir \".\"\n");

        assert_eq!(args, vec!["./tools/my-tool.exe", "workdir", "."]);
    }

    #[test]
    fn parses_existing_exec_identifier_syntax() {
        let args = parse_exec_args("exec cargo test retry 3 timeout 30s\n");

        assert_eq!(
            args,
            vec!["cargo", "test", "retry", "3", "timeout", "30", "s"]
        );
    }

    #[test]
    fn parses_sleep_command_style_call() {
        let program = parse("sleep 10ms\n");

        match program.statements.as_slice() {
            [Stmt::Expr(Expr::Call(call))] => {
                assert_eq!(call.name, vec!["sleep".to_string()]);
                assert_eq!(call.args.len(), 2);
            }
            _ => panic!("Expected sleep expression"),
        }
    }

    #[test]
    fn parses_shell_like_builtin_command_style_calls() {
        for command in ["cd src", "pwd", "which clear", "help time.format", "clear"] {
            let program = parse(&format!("{}\n", command));

            match program.statements.as_slice() {
                [Stmt::Expr(Expr::Call(_))] => {}
                _ => panic!("Expected command-style expression for {command}"),
            }
        }
    }
}
