use crate::lexer::Token;
pub struct Program {
    pub requires: Vec<(String, String)>,
    pub statements: Vec<Stmt>,
}


pub enum Stmt {
    Assignment { name: String, expr: Expr },
    Pipeline(Pipeline),
    Expr(Expr),
}


pub struct Pipeline {
    pub base: Expr,
    pub stages: Vec<PipeStage>,
}


pub enum PipeStage {    
    Where { expr: Expr,},
    Select { fields: Vec<String> },
    Sort { field: String, descending: bool },
    Limit { count: usize },
    Count,
    Sum { field: String },
    Avg { field: String },
    Max { field: String },
    Min { field: String },
    Distinct { field: String },        
    Call(FunctionCall),
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
    Number(f64),
    String(String),
    Bool(bool),

    Call(FunctionCall),
    Literal(Literal),
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

#[derive(Clone)]
pub struct FunctionCall {
    pub name: Vec<String>,
    pub args: Vec<Expr>,
}

#[derive(Debug,Clone)]
pub enum Literal {
    String(String),
    Number(f64),
    Bool(bool),
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

