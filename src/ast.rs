#![allow(dead_code)]

use crate::lexer::Token;

pub struct Program {
    pub requires: Vec<(String, String)>,
    pub statements: Vec<Stmt>,
}

pub enum Stmt {
    Requires(Vec<String>),

    Assignment {
        name: String,
        value: AssignmentValue,
    },

    If {
        condition: Expr,
        then_branch: Vec<Stmt>,
        else_branch: Vec<Stmt>,
    },

    Try {
        try_branch: Vec<Stmt>,
        catch_name: Option<String>,
        catch_branch: Vec<Stmt>,
        finally_branch: Vec<Stmt>,
    },

    Pipeline(Pipeline),

    Expr(Expr),
}

pub enum AssignmentValue {
    Expr(Expr),
    Pipeline(Pipeline),
}

pub struct Pipeline {
    pub base: Expr,
    pub stages: Vec<PipeStage>,
}

#[derive(Debug, Clone)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,

    Gt,
    Lt,
    Gte,
    Lte,
    Eq,
    Neq,

    And,
    Or,
}

#[derive(Clone)]
pub enum Expr {
    Ident(String),
    Variable(String),
    Number(f64),
    String(String),
    Bool(bool),
    Literal(Literal),
    List(Vec<Expr>),
    Object(Vec<(String, Expr)>),

    Call(FunctionCall),

    Binary {
        left: Box<Expr>,
        op: BinOp,
        right: Box<Expr>,
    },
    Unary {
        op: Token,
        expr: Box<Expr>,
    },
}
#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Int(i64),
    Float(f64),
    Number(f64),
    Bool(bool),
    String(String),
    Null,
}

#[derive(Debug)]
pub enum Op {
    Gt,
    Lt,
    Gte,
    Lte,
    Eq,
    Neq,
}

#[derive(Debug, Clone)]
pub struct SpannedToken {
    pub token: Token,
    pub line: usize,
    pub col: usize,
}
pub enum PipelineStage {
    Where(Expr),
    Select(Vec<String>),
}

pub enum PipeStage {
    // 🔍 Filter rows
    Where { expr: Expr },

    // 📦 Select fields
    Select { fields: Vec<String> },

    // 📦 Strict field filtering
    Fields { fields: Vec<String> },

    // 🔎 Extract one field
    Get { field: String },

    // 🔁 Data format conversions
    ToJson,

    FromJson,

    // 🖨️ Render as a table
    Table,

    // 💾 Save pipeline input to a file
    Save { path: String },

    // 🔽 Sort by field
    Sort { field: String, descending: bool },

    // 🔢 Limit number of results
    Limit { count: usize },

    // 🔢 Aggregations
    Count,

    Avg { field: String },

    Sum { field: String },

    Max { field: String },

    Min { field: String },

    // 🧹 Remove duplicates
    Distinct { field: String },

    // 🔌 Custom function / plugin stage
    Call(FunctionCall),
}
#[derive(Clone)]
pub struct FunctionCall {
    pub name: Vec<String>, // e.g. ["fs", "read"]
    pub args: Vec<Expr>,
    pub config: Option<CallConfig>,
}

#[derive(Clone)]
pub struct CallConfig {
    pub env: Vec<(String, Expr)>,
}

impl Expr {
    pub fn int(value: i64) -> Self {
        Expr::Literal(Literal::Int(value))
    }

    pub fn float(value: f64) -> Self {
        Expr::Literal(Literal::Float(value))
    }

    pub fn string(value: impl Into<String>) -> Self {
        Expr::Literal(Literal::String(value.into()))
    }

    pub fn bool(value: bool) -> Self {
        Expr::Literal(Literal::Bool(value))
    }

    pub fn null() -> Self {
        Expr::Literal(Literal::Null)
    }
}
