use crate::ast::Expr;
use crate::ast::*;
use crate::lexer::{Lexer, Token};
use crate::parser::Parser;
use crate::permissions::PermissionSet;
use crate::runtime::plugin::{CommandDoc, PluginHost, PluginResult, ZenPlugin};
use zen_runtime::capabilities::{CapabilityGrant, Capabilities};
use zen_runtime::effects::{Effect, Effects, ProcessEffects};
use crate::runtime::plugins::external::{
    discover_external_plugin_manifests, external_plugin_diagnostics,
    load_external_plugin_from_path, record_external_plugin_loaded, record_external_plugin_unloaded,
    ExternalPluginDiagnostics,
};
use crate::runtime::plugins::registry::builtin_plugins;
use crate::runtime::process::{exec_command, parse_duration, ExecRequest};
use crate::runtime::script_runner::ScriptRunner;
use crate::runtime::secret_store::SecretStore;
use crate::runtime::time::{duration_summary, parse_time_reference};
use crate::runtime::values::Value;
use crate::terminal;
use chrono::{DateTime, Local, TimeZone, Utc};
use serde_json::{Map as JsonMap, Value as JsonValue};
use zen_runtime::values::{eq_vals, json_to_value, value_to_echo_string, value_to_json};
use zen_runtime::workflow::{self, WorkflowEngine};
use zen_runtime::workflow_host::WorkflowHost;
use std::collections::{HashMap, HashSet};
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use sysinfo::System;

pub struct Executor {
    env: HashMap<String, Value>,
    permissions: PermissionSet,
    ctx: Context,
    mocked_time: Option<DateTime<Utc>>,
    workspace_root: PathBuf,
    cwd: PathBuf,
    plugins: Vec<Arc<dyn ZenPlugin>>,
}

pub struct Context {
    pub vars: HashMap<String, Value>,
}

impl Context {
    pub fn new() -> Self {
        Self {
            vars: HashMap::new(),
        }
    }
}

impl Executor {
    pub fn new_with_permissions(permissions: PermissionSet) -> Self {
        Self::new_with_plugins(permissions, builtin_plugins())
    }

    pub fn new_with_permissions_and_workspace(
        permissions: PermissionSet,
        workspace_root: Option<PathBuf>,
    ) -> Result<Self, String> {
        Self::new_with_plugins_and_workspace(permissions, builtin_plugins(), workspace_root)
    }

    pub fn new_with_plugins(mut permissions: PermissionSet, plugins: Vec<Arc<dyn ZenPlugin>>) -> Self {
        // `.fg` has always treated time (and any future randomness builtin)
        // as ambient, ungated authority - auto-grant both here so they now
        // formally exist as capability kinds (for a future static checker
        // like Flux to gate on) without changing today's behavior at all.
        permissions.grant(CapabilityGrant::new("time"));
        permissions.grant(CapabilityGrant::new("rand"));

        let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let workspace_root = Self::discover_workspace_root(&cwd);

        Self {
            env: HashMap::new(),
            permissions,
            ctx: Context::new(),
            mocked_time: None,
            workspace_root,
            cwd,
            plugins,
        }
    }

    pub fn new_with_plugins_and_workspace(
        permissions: PermissionSet,
        plugins: Vec<Arc<dyn ZenPlugin>>,
        workspace_root: Option<PathBuf>,
    ) -> Result<Self, String> {
        let mut executor = Self::new_with_plugins(permissions, plugins);

        if let Some(workspace_root) = workspace_root {
            let root = fs::canonicalize(&workspace_root).map_err(|e| {
                format!(
                    "Invalid workspace '{}': {}",
                    workspace_root.to_string_lossy(),
                    e
                )
            })?;
            let root = Self::normalize_canonical_path(root);

            if !root.is_dir() {
                return Err(format!(
                    "Invalid workspace '{}': not a directory",
                    workspace_root.to_string_lossy()
                ));
            }

            let cwd = env::current_dir().unwrap_or_else(|_| root.clone());
            executor.cwd = if cwd.starts_with(&root) {
                cwd
            } else {
                root.clone()
            };
            executor.workspace_root = root;
        }

        Ok(executor)
    }

    pub fn grant_permissions(&mut self, required: &[(String, String)]) {
        self.permissions.extend(required);
    }

    pub fn reset_session(&mut self) {
        self.env.clear();
        self.ctx = Context::new();
        self.permissions = PermissionSet::new(&Vec::new());
        self.mocked_time = None;
    }

    pub fn reload_plugins(&mut self) {
        self.plugins = builtin_plugins();
    }

    pub fn permissions(&self) -> Vec<String> {
        self.permissions.list()
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn plugin_inventory(&self) -> Result<Value, String> {
        self.plugins_list()
    }

    pub fn external_plugin_diagnostics(&self) -> ExternalPluginDiagnostics {
        external_plugin_diagnostics()
    }

    pub fn variables(&self) -> Vec<(&str, &Value)> {
        let mut vars: Vec<_> = self
            .ctx
            .vars
            .iter()
            .map(|(name, value)| (name.as_str(), value))
            .collect();
        vars.sort_by(|left, right| left.0.cmp(right.0));
        vars
    }

    pub(crate) fn plugin_arg_value(&mut self, expr: Expr) -> Result<Value, String> {
        self.eval_echo_arg(expr)
    }

    pub(crate) fn check_permission(&self, permission: &str) -> Result<(), String> {
        self.permissions.check(permission)
    }

    pub fn execute(&mut self, program: Program) -> Result<(), String> {
        for stmt in program.statements {
            self.execute_stmt(stmt)?;
        }
        Ok(())
    }

    pub fn execute_capture(&mut self, program: Program) -> Result<Value, String> {
        let mut last = Value::Null;
        for stmt in program.statements {
            last = self.execute_stmt_capture(stmt)?;
        }
        Ok(last)
    }

    fn execute_stmt(&mut self, stmt: Stmt) -> Result<(), String> {
        match stmt {
            Stmt::Requires(_caps) => {
                //self.capabilities = caps;
                Ok(())
            }

            Stmt::Assignment { name, value } => {
                let value = self.eval_assignment_value(value)?;
                self.ctx.vars.insert(name.clone(), value.clone());
                self.env.insert(name, value);
                Ok(())
            }

            Stmt::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let condition = self.eval_value_expr(&condition)?;
                let Value::Bool(condition) = condition else {
                    return Err("if condition must evaluate to a boolean".into());
                };

                let branch = if condition { then_branch } else { else_branch };
                for stmt in branch {
                    self.execute_stmt(stmt)?;
                }

                Ok(())
            }

            Stmt::Try {
                try_branch,
                catch_name,
                catch_branch,
                finally_branch,
            } => {
                let try_result = self.execute_block(try_branch);
                let handled_result = match try_result {
                    Ok(()) => Ok(()),
                    Err(error) if !catch_branch.is_empty() => {
                        if let Some(name) = catch_name {
                            let value = Value::String(error);
                            self.ctx.vars.insert(name.clone(), value.clone());
                            self.env.insert(name, value);
                        }
                        self.execute_block(catch_branch)
                    }
                    Err(error) => Err(error),
                };

                match self.execute_block(finally_branch) {
                    Ok(()) => handled_result,
                    Err(finally_error) => Err(finally_error),
                }
            }

            Stmt::Pipeline(pipeline) => {
                let result = self.eval_pipeline(pipeline)?;

                Self::print_result(&result);

                Ok(())
            }
            Stmt::Expr(expr) => {
                let result = self.eval_value_expr(&expr)?;
                Self::print_result(&result);
                Ok(())
            }
        }
    }

    fn execute_stmt_capture(&mut self, stmt: Stmt) -> Result<Value, String> {
        match stmt {
            Stmt::Requires(_caps) => Ok(Value::Null),
            Stmt::Assignment { name, value } => {
                let value = self.eval_assignment_value(value)?;
                self.ctx.vars.insert(name.clone(), value.clone());
                self.env.insert(name, value.clone());
                Ok(value)
            }
            Stmt::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let condition = self.eval_value_expr(&condition)?;
                let Value::Bool(condition) = condition else {
                    return Err("if condition must evaluate to a boolean".into());
                };

                let branch = if condition { then_branch } else { else_branch };
                let mut last = Value::Null;
                for stmt in branch {
                    last = self.execute_stmt_capture(stmt)?;
                }
                Ok(last)
            }
            Stmt::Try {
                try_branch,
                catch_name,
                catch_branch,
                finally_branch,
            } => {
                let try_result = self.execute_block_capture(try_branch);
                let handled_result = match try_result {
                    Ok(value) => Ok(value),
                    Err(error) if !catch_branch.is_empty() => {
                        if let Some(name) = catch_name {
                            let value = Value::String(error);
                            self.ctx.vars.insert(name.clone(), value.clone());
                            self.env.insert(name, value);
                        }
                        self.execute_block_capture(catch_branch)
                    }
                    Err(error) => Err(error),
                };

                match self.execute_block_capture(finally_branch) {
                    Ok(Value::Null) => handled_result,
                    Ok(value) => Ok(value),
                    Err(finally_error) => Err(finally_error),
                }
            }
            Stmt::Pipeline(pipeline) => self.eval_pipeline(pipeline),
            Stmt::Expr(expr) => self.eval_value_expr(&expr),
        }
    }

    fn execute_block(&mut self, statements: Vec<Stmt>) -> Result<(), String> {
        for stmt in statements {
            self.execute_stmt(stmt)?;
        }
        Ok(())
    }

    fn execute_block_capture(&mut self, statements: Vec<Stmt>) -> Result<Value, String> {
        let mut last = Value::Null;
        for stmt in statements {
            last = self.execute_stmt_capture(stmt)?;
        }
        Ok(last)
    }

    fn eval_assignment_value(&mut self, value: AssignmentValue) -> Result<Value, String> {
        match value {
            AssignmentValue::Expr(expr) => self.eval_value_expr(&expr),
            AssignmentValue::Pipeline(pipeline) => self.eval_pipeline(pipeline),
        }
    }

    fn eval_value_expr(&mut self, expr: &Expr) -> Result<Value, String> {
        match expr {
            Expr::Call(call) if call.args.is_empty() && call.name.len() > 1 => {
                match self.resolve_path(&call.name) {
                    Some(value) => value,
                    None => self.eval_call(call.clone()),
                }
            }
            Expr::Call(call) => self.eval_call(call.clone()),
            Expr::List(items) => items
                .iter()
                .map(|item| self.eval_value_expr(item))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::List),
            Expr::Object(entries) => {
                let mut object = HashMap::new();
                for (key, expr) in entries {
                    object.insert(key.clone(), self.eval_value_expr(expr)?);
                }
                Ok(Value::Object(object))
            }
            Expr::Unary { op, expr } => {
                let val = self.eval_value_expr(expr)?;

                match op {
                    Token::Minus => match val {
                        Value::Number(n) => Ok(Value::Number(-n)),
                        _ => Err("Unary '-' expects number".into()),
                    },
                    Token::NotEq | Token::Bang => match val {
                        Value::Bool(b) => Ok(Value::Bool(!b)),
                        _ => Err("Unary '!' expects bool".into()),
                    },
                    _ => Err(format!("Unsupported unary operator {:?}", op)),
                }
            }
            Expr::Binary { left, op, right } => {
                let l = self.eval_value_expr(left)?;
                let r = self.eval_value_expr(right)?;

                match op {
                    BinOp::Add => match (l, r) {
                        (Value::Number(a), Value::Number(b)) => Ok(Value::Number(a + b)),
                        _ => Err("Invalid '+' operands".into()),
                    },
                    BinOp::Sub => match (l, r) {
                        (Value::Number(a), Value::Number(b)) => Ok(Value::Number(a - b)),
                        _ => Err("Invalid '-' operands".into()),
                    },
                    BinOp::Mul => match (l, r) {
                        (Value::Number(a), Value::Number(b)) => Ok(Value::Number(a * b)),
                        _ => Err("Invalid '*' operands".into()),
                    },
                    BinOp::Div => match (l, r) {
                        (Value::Number(a), Value::Number(b)) => Ok(Value::Number(a / b)),
                        _ => Err("Invalid '/' operands".into()),
                    },
                    BinOp::Gt => Self::cmp(l, r, |a, b| a > b),
                    BinOp::Lt => Self::cmp(l, r, |a, b| a < b),
                    BinOp::Gte => Self::cmp(l, r, |a, b| a >= b),
                    BinOp::Lte => Self::cmp(l, r, |a, b| a <= b),
                    BinOp::Eq => Ok(Value::Bool(eq_vals(&l, &r))),
                    BinOp::Neq => Ok(Value::Bool(!eq_vals(&l, &r))),
                    BinOp::And => match (l, r) {
                        (Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(a && b)),
                        _ => Err("Invalid '&&' operands".into()),
                    },
                    BinOp::Or => match (l, r) {
                        (Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(a || b)),
                        _ => Err("Invalid '||' operands".into()),
                    },
                }
            }
            other => Self::eval_expr(other, &mut self.ctx),
        }
    }

    fn print_result(value: &Value) {
        if !matches!(value, Value::Null) {
            match value {
                Value::Object(_) | Value::List(_) => {
                    match serde_json::to_string_pretty(&value_to_json(value)) {
                        Ok(json) => println!("{}", json),
                        Err(_) => println!("{}", value_to_echo_string(value.clone())),
                    }
                }
                _ => println!("{}", value_to_echo_string(value.clone())),
            }
        }
    }

    pub fn eval_expr(expr: &Expr, ctx: &mut Context) -> Result<Value, String> {
        match expr {
            Expr::Number(n) => Ok(Value::Number(*n)),
            Expr::String(s) => Ok(Value::String(s.clone())),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::List(items) => items
                .iter()
                .map(|item| Self::eval_expr(item, ctx))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::List),
            Expr::Object(entries) => {
                let mut object = HashMap::new();
                for (key, expr) in entries {
                    object.insert(key.clone(), Self::eval_expr(expr, ctx)?);
                }
                Ok(Value::Object(object))
            }
            Expr::Ident(name) => ctx
                .vars
                .get(name)
                .cloned()
                .ok_or_else(|| format!("Undefined variable '{}'", name)),
            Expr::Variable(name) => ctx
                .vars
                .get(name)
                .cloned()
                .ok_or_else(|| format!("Undefined variable '${}'", name)),
            Expr::Call(call) => Err(format!(
                "Function calls are not supported in this expression context: {}",
                call.name.join(".")
            )),

            Expr::Literal(lit) => match lit {
                Literal::Int(i) => Ok(Value::Number(*i as f64)),
                Literal::Float(f) => Ok(Value::Number(*f)),
                Literal::Number(n) => Ok(Value::Number(*n)),
                Literal::Bool(b) => Ok(Value::Bool(*b)),
                Literal::String(s) => Ok(Value::String(s.clone())),
                Literal::Null => Ok(Value::Null),
            },

            Expr::Unary { op, expr } => {
                let val = Self::eval_expr(expr, ctx)?;

                match op {
                    Token::Minus => match val {
                        Value::Number(n) => Ok(Value::Number(-n)),
                        _ => Err("Unary '-' expects number".into()),
                    },

                    Token::NotEq => match val {
                        Value::Bool(b) => Ok(Value::Bool(!b)),
                        _ => Err("Unary '!' expects bool".into()),
                    },
                    Token::Bang => match val {
                        Value::Bool(b) => Ok(Value::Bool(!b)),
                        _ => Err("Unary '!' expects bool".into()),
                    },

                    _ => Err(format!("Unsupported unary operator {:?}", op)),
                }
            }

            Expr::Binary { left, op, right } => {
                let l = Self::eval_expr(left, ctx)?;
                let r = Self::eval_expr(right, ctx)?;

                match op {
                    // Arithmetic
                    BinOp::Add => match (l, r) {
                        (Value::Number(a), Value::Number(b)) => Ok(Value::Number(a + b)),
                        _ => Err("Invalid '+' operands".into()),
                    },

                    BinOp::Sub => match (l, r) {
                        (Value::Number(a), Value::Number(b)) => Ok(Value::Number(a - b)),
                        _ => Err("Invalid '-' operands".into()),
                    },

                    BinOp::Mul => match (l, r) {
                        (Value::Number(a), Value::Number(b)) => Ok(Value::Number(a * b)),
                        _ => Err("Invalid '*' operands".into()),
                    },

                    BinOp::Div => match (l, r) {
                        (Value::Number(a), Value::Number(b)) => Ok(Value::Number(a / b)),
                        _ => Err("Invalid '/' operands".into()),
                    },

                    // Comparison
                    BinOp::Gt => Self::cmp(l, r, |a, b| a > b),
                    BinOp::Lt => Self::cmp(l, r, |a, b| a < b),
                    BinOp::Gte => Self::cmp(l, r, |a, b| a >= b),
                    BinOp::Lte => Self::cmp(l, r, |a, b| a <= b),

                    // Equality
                    BinOp::Eq => Ok(Value::Bool(eq_vals(&l, &r))),
                    BinOp::Neq => Ok(Value::Bool(!eq_vals(&l, &r))),

                    // Logical
                    BinOp::And => match (l, r) {
                        (Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(a && b)),
                        _ => Err("Invalid '&&' operands".into()),
                    },

                    BinOp::Or => match (l, r) {
                        (Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(a || b)),
                        _ => Err("Invalid '||' operands".into()),
                    },
                }
            }
        }
    }

    fn literal_to_value(&self, lit: Literal) -> Value {
        match lit {
            Literal::Int(i) => Value::Number(i as f64),
            Literal::Float(f) => Value::Number(f),
            Literal::String(s) => Value::String(s),
            Literal::Number(n) => Value::Number(n),
            Literal::Bool(b) => Value::Bool(b),
            Literal::Null => Value::Null,
        }
    }

    fn eval_pipeline(&mut self, pipeline: Pipeline) -> Result<Value, String> {
        let mut value = match pipeline.base {
            Expr::Call(call) => self.eval_call_with_input(call, Value::Null)?,
            expr => self.eval_value_expr(&expr)?,
        };

        for stage in pipeline.stages {
            value = match stage {
                PipeStage::Where { expr } => self.pipe_where(value, expr)?,
                PipeStage::Select { fields } => self.pipe_select(value, fields)?,
                PipeStage::Fields { fields } => self.pipe_fields(value, fields)?,
                PipeStage::Get { field } => self.pipe_get(value, field)?,
                PipeStage::ToJson => self.pipe_to_json(value)?,
                PipeStage::FromJson => self.pipe_from_json(value)?,
                PipeStage::Table => self.pipe_table(value)?,
                PipeStage::Save { path } => self.pipe_save(value, path)?,
                PipeStage::Sort { field, descending } => {
                    self.pipe_sort(value, field, descending)?
                }
                PipeStage::Limit { count } => self.pipe_limit(value, count)?,
                PipeStage::Count => self.pipe_count(value)?,
                PipeStage::Sum { field } => self.pipe_sum(value, field)?,
                PipeStage::Avg { field } => self.pipe_avg(value, field)?,
                PipeStage::Max { field } => self.pipe_max(value, field)?,
                PipeStage::Min { field } => self.pipe_min(value, field)?,
                PipeStage::Distinct { field } => self.pipe_distinct(value, field)?,
                PipeStage::Call(call) => self.eval_call_with_input(call, value)?,
            };
        }

        Ok(value)
    }

    fn pipe_where(&self, input: Value, expr: Expr) -> Result<Value, String> {
        if let Value::List(items) = input {
            let filtered = items
                .into_iter()
                .filter(|item| match self.eval_expr_on_item(expr.clone(), item) {
                    Value::Bool(b) => b,
                    _ => false,
                })
                .collect();

            Ok(Value::List(filtered))
        } else {
            Err("where can only operate on lists".into())
        }
    }

    #[allow(dead_code)]
    fn eval_where_expr(&self, expr: Expr, item: &Value) -> bool {
        match expr {
            Expr::Binary { left, op, right } => {
                let l = self.eval_expr_on_item(*left, item);
                let r = self.eval_expr_on_item(*right, item);

                match (l, r, op) {
                    (Value::Number(a), Value::Number(b), BinOp::Gt) => a > b,
                    (Value::Number(a), Value::Number(b), BinOp::Lt) => a < b,
                    (Value::Number(a), Value::Number(b), BinOp::Gte) => a >= b,
                    (Value::Number(a), Value::Number(b), BinOp::Lte) => a <= b,
                    (Value::Number(a), Value::Number(b), BinOp::Eq) => a == b,
                    (Value::Number(a), Value::Number(b), BinOp::Neq) => a != b,

                    (Value::Bool(a), Value::Bool(b), BinOp::And) => a && b,
                    (Value::Bool(a), Value::Bool(b), BinOp::Or) => a || b,

                    _ => false,
                }
            }
            _ => false,
        }
    }

    fn eval_expr_on_item(&self, expr: Expr, item: &Value) -> Value {
        match expr {
            Expr::Ident(name) => {
                if let Value::Object(map) = item {
                    map.get(&name).cloned().unwrap_or(Value::Null)
                } else {
                    Value::Null
                }
            }
            Expr::Variable(name) => {
                if let Value::Object(map) = item {
                    map.get(&name).cloned().unwrap_or(Value::Null)
                } else {
                    Value::Null
                }
            }

            Expr::Literal(l) => self.literal_to_value(l),
            Expr::Number(value) => Value::Number(value),
            Expr::String(value) => Value::String(value),
            Expr::Bool(value) => Value::Bool(value),

            Expr::Binary { left, op, right } => {
                let l = self.eval_expr_on_item(*left, item);
                let r = self.eval_expr_on_item(*right, item);

                self.eval_binary_op(l, op, r)
            }

            _ => Value::Null,
        }
    }

    fn eval_binary_op(&self, left: Value, op: BinOp, right: Value) -> Value {
        match (left, right, op) {
            // Arithmetic
            (Value::Number(a), Value::Number(b), BinOp::Add) => Value::Number(a + b),
            (Value::Number(a), Value::Number(b), BinOp::Sub) => Value::Number(a - b),
            (Value::Number(a), Value::Number(b), BinOp::Mul) => Value::Number(a * b),
            (Value::Number(a), Value::Number(b), BinOp::Div) => {
                if b == 0.0 {
                    Value::Null
                } else {
                    Value::Number(a / b)
                }
            }

            // Comparison
            (Value::Number(a), Value::Number(b), BinOp::Gt) => Value::Bool(a > b),
            (Value::Number(a), Value::Number(b), BinOp::Lt) => Value::Bool(a < b),
            (Value::Number(a), Value::Number(b), BinOp::Gte) => Value::Bool(a >= b),
            (Value::Number(a), Value::Number(b), BinOp::Lte) => Value::Bool(a <= b),
            (Value::Number(a), Value::Number(b), BinOp::Eq) => Value::Bool(a == b),
            (Value::Number(a), Value::Number(b), BinOp::Neq) => Value::Bool(a != b),

            // Boolean
            (Value::Bool(a), Value::Bool(b), BinOp::And) => Value::Bool(a && b),
            (Value::Bool(a), Value::Bool(b), BinOp::Or) => Value::Bool(a || b),
            (Value::Bool(a), Value::Bool(b), BinOp::Eq) => Value::Bool(a == b),
            (Value::Bool(a), Value::Bool(b), BinOp::Neq) => Value::Bool(a != b),

            // String equality
            (Value::String(a), Value::String(b), BinOp::Eq) => Value::Bool(a == b),
            (Value::String(a), Value::String(b), BinOp::Neq) => Value::Bool(a != b),

            _ => Value::Null,
        }
    }

    fn pipe_select(&self, input: Value, fields: Vec<String>) -> Result<Value, String> {
        match input {
            Value::Object(map) => Ok(Value::Object(Self::select_object_fields(map, &fields))),
            Value::List(items) => {
                let projected = items
                    .into_iter()
                    .map(|item| {
                        if let Value::Object(map) = item {
                            Value::Object(Self::select_object_fields(map, &fields))
                        } else {
                            item
                        }
                    })
                    .collect();

                Ok(Value::List(projected))
            }
            _ => Err("select can only operate on objects or lists".into()),
        }
    }

    fn pipe_fields(&self, input: Value, fields: Vec<String>) -> Result<Value, String> {
        match input {
            Value::Object(map) => Ok(Value::Object(Self::select_object_fields(map, &fields))),
            Value::List(items) => {
                let mut projected = Vec::new();
                for item in items {
                    let Value::Object(map) = item else {
                        return Err("fields expects an object or list of objects".into());
                    };
                    projected.push(Value::Object(Self::select_object_fields(map, &fields)));
                }

                Ok(Value::List(projected))
            }
            _ => Err("fields expects an object or list of objects".into()),
        }
    }

    fn pipe_get(&self, input: Value, field: String) -> Result<Value, String> {
        match input {
            Value::Object(map) => Ok(map.get(&field).cloned().unwrap_or(Value::Null)),
            Value::List(items) => {
                let mut values = Vec::new();
                for item in items {
                    let Value::Object(map) = item else {
                        return Err("get expects an object or list of objects".into());
                    };
                    values.push(map.get(&field).cloned().unwrap_or(Value::Null));
                }
                Ok(Value::List(values))
            }
            _ => Err("get expects an object or list of objects".into()),
        }
    }

    fn pipe_to_json(&self, input: Value) -> Result<Value, String> {
        serde_json::to_string_pretty(&value_to_json(&input))
            .map(Value::String)
            .map_err(|error| format!("Failed to encode JSON: {}", error))
    }

    fn pipe_from_json(&self, input: Value) -> Result<Value, String> {
        self.parse_json(input)
    }

    fn pipe_table(&self, input: Value) -> Result<Value, String> {
        print!("{}", Self::value_to_table(&input));
        Ok(Value::Null)
    }

    fn pipe_save(&self, input: Value, path: String) -> Result<Value, String> {
        self.permissions.check("fs.write")?;
        let resolved = self.resolve_local_write_path(&path)?;
        if let Some(parent) = resolved.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("Failed to create '{}': {}", parent.display(), error))?;
        }

        let text = match input {
            Value::String(value) => value,
            value => serde_json::to_string_pretty(&value_to_json(&value))
                .map_err(|error| format!("Failed to encode JSON: {}", error))?,
        };
        fs::write(&resolved, &text)
            .map_err(|error| format!("Failed to save '{}': {}", path, error))?;

        let mut map = HashMap::new();
        map.insert("saved".into(), Value::Bool(true));
        map.insert(
            "path".into(),
            Value::String(resolved.to_string_lossy().into()),
        );
        map.insert("bytes".into(), Value::Number(text.len() as f64));
        Ok(Value::Object(map))
    }

    fn select_object_fields(
        map: HashMap<String, Value>,
        fields: &[String],
    ) -> HashMap<String, Value> {
        let mut new_map = HashMap::new();
        for field in fields {
            if let Some(value) = map.get(field) {
                new_map.insert(field.clone(), value.clone());
            }
        }
        new_map
    }

    fn pipe_sort(&self, input: Value, field: String, descending: bool) -> Result<Value, String> {
        if let Value::List(mut items) = input {
            items.sort_by(|a, b| {
                let va = match a {
                    Value::Object(map) => map.get(&field),
                    _ => None,
                };

                let vb = match b {
                    Value::Object(map) => map.get(&field),
                    _ => None,
                };

                let ord = match (va, vb) {
                    (Some(Value::Number(x)), Some(Value::Number(y))) => {
                        x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal)
                    }
                    (Some(Value::String(x)), Some(Value::String(y))) => x.cmp(y),
                    _ => std::cmp::Ordering::Equal,
                };

                if descending {
                    ord.reverse()
                } else {
                    ord
                }
            });

            Ok(Value::List(items))
        } else {
            Err("sort can only operate on lists".into())
        }
    }

    fn pipe_limit(&self, input: Value, count: usize) -> Result<Value, String> {
        if let Value::List(mut items) = input {
            if items.len() > count {
                items.truncate(count);
            }
            Ok(Value::List(items))
        } else {
            Err("limit can only operate on lists".into())
        }
    }

    fn pipe_count(&self, input: Value) -> Result<Value, String> {
        if let Value::List(items) = input {
            Ok(Value::Number(items.len() as f64))
        } else {
            Err("count can only operate on lists".into())
        }
    }

    fn pipe_max(&self, input: Value, field: String) -> Result<Value, String> {
        if let Value::List(items) = input {
            let mut max_val: Option<f64> = None;

            for item in items {
                if let Value::Object(map) = item {
                    if let Some(Value::Number(n)) = map.get(&field) {
                        max_val = Some(match max_val {
                            Some(current) => current.max(*n),
                            None => *n,
                        });
                    }
                }
            }

            Ok(Value::Number(max_val.unwrap_or(0.0)))
        } else {
            Err("max can only operate on lists".into())
        }
    }

    fn pipe_min(&self, input: Value, field: String) -> Result<Value, String> {
        if let Value::List(items) = input {
            let mut min_val: Option<f64> = None;

            for item in items {
                if let Value::Object(map) = item {
                    if let Some(Value::Number(n)) = map.get(&field) {
                        min_val = Some(match min_val {
                            Some(current) => current.min(*n),
                            None => *n,
                        });
                    }
                }
            }

            Ok(Value::Number(min_val.unwrap_or(0.0)))
        } else {
            Err("min can only operate on lists".into())
        }
    }

    fn pipe_sum(&self, input: Value, field: String) -> Result<Value, String> {
        if let Value::List(items) = input {
            let mut total = 0.0;

            for item in items {
                if let Value::Object(map) = item {
                    if let Some(Value::Number(n)) = map.get(&field) {
                        total += *n;
                    }
                }
            }

            Ok(Value::Number(total))
        } else {
            Err("sum can only operate on lists".into())
        }
    }

    fn pipe_avg(&self, input: Value, field: String) -> Result<Value, String> {
        if let Value::List(items) = input {
            let mut total = 0.0;
            let mut count = 0.0;

            for item in items {
                if let Value::Object(map) = item {
                    if let Some(Value::Number(n)) = map.get(&field) {
                        total += *n;
                        count += 1.0;
                    }
                }
            }

            if count == 0.0 {
                return Ok(Value::Number(0.0));
            }

            Ok(Value::Number(total / count))
        } else {
            Err("avg can only operate on lists".into())
        }
    }

    fn pipe_distinct(&self, input: Value, field: String) -> Result<Value, String> {
        if let Value::List(items) = input {
            let mut seen = HashSet::new();
            let mut result = Vec::new();

            for item in items {
                if let Value::Object(map) = &item {
                    if let Some(val) = map.get(&field) {
                        let key = format!("{:?}", val);

                        if !seen.contains(&key) {
                            seen.insert(key);
                            result.push(item);
                        }
                    }
                }
            }

            Ok(Value::List(result))
        } else {
            Err("distinct can only operate on lists".into())
        }
    }

    #[allow(dead_code)]
    fn compare(&self, left: &Value, op: &Op, right: &Value) -> bool {
        match (left, right) {
            (Value::Number(a), Value::Number(b)) => match op {
                Op::Gt => a > b,
                Op::Lt => a < b,
                Op::Gte => a >= b,
                Op::Lte => a <= b,
                Op::Eq => a == b,
                Op::Neq => a != b,
            },
            _ => false,
        }
    }

    fn eval_call(&mut self, call: FunctionCall) -> Result<Value, String> {
        self.eval_call_with_input(call, Value::Null)
    }

    fn eval_call_with_input(&mut self, call: FunctionCall, input: Value) -> Result<Value, String> {
        let name = call.name.join(".");

        if let Some(value) = self.dispatch_plugins(&call, &input)? {
            return Ok(value);
        }

        Err(format!("Unknown function {}", name))
    }

    fn dispatch_plugins(
        &mut self,
        call: &FunctionCall,
        input: &Value,
    ) -> Result<Option<Value>, String> {
        let plugins = self.plugins.clone();

        for plugin in plugins {
            match plugin.call(self, call, input)? {
                PluginResult::Handled(value) => return Ok(Some(value)),
                PluginResult::Unhandled => {}
            }
        }

        Ok(None)
    }

    pub(crate) fn core_echo(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String> {
        self.echo(args, input)
    }

    pub(crate) fn core_parse(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String> {
        self.parse_value(args, input)
    }

    pub(crate) fn core_which(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        self.which(args)
    }

    pub(crate) fn core_clear(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        self.clear(args)
    }

    pub(crate) fn core_cd(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        self.cd(args)
    }

    pub(crate) fn core_pwd(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        self.pwd(args)
    }

    pub(crate) fn plugins_list(&self) -> Result<Value, String> {
        let plugins = self
            .plugins
            .iter()
            .map(|plugin| {
                let mut map = HashMap::new();
                let command_permissions = plugin.command_permissions();
                let mut permission_names = Vec::new();
                let mut seen_permissions = HashSet::new();

                for (_, permission) in command_permissions {
                    if seen_permissions.insert(*permission) {
                        permission_names.push(Value::String((*permission).into()));
                    }
                }

                map.insert("name".into(), Value::String(plugin.name().into()));
                map.insert(
                    "description".into(),
                    plugin
                        .description()
                        .map(|value| Value::String(value.into()))
                        .unwrap_or(Value::Null),
                );
                map.insert(
                    "version".into(),
                    plugin
                        .version()
                        .map(|value| Value::String(value.into()))
                        .unwrap_or(Value::Null),
                );
                map.insert(
                    "author".into(),
                    plugin
                        .author()
                        .map(|value| Value::String(value.into()))
                        .unwrap_or(Value::Null),
                );
                map.insert(
                    "homepage".into(),
                    plugin
                        .homepage()
                        .map(|value| Value::String(value.into()))
                        .unwrap_or(Value::Null),
                );
                map.insert("kind".into(), Value::String(plugin.kind().into()));
                map.insert(
                    "source".into(),
                    plugin
                        .source()
                        .map(|source| Value::String(source.into()))
                        .unwrap_or(Value::Null),
                );
                map.insert(
                    "commands".into(),
                    Value::List(
                        plugin
                            .commands()
                            .iter()
                            .map(|command| Value::String((*command).into()))
                            .collect(),
                    ),
                );
                map.insert(
                    "command_count".into(),
                    Value::Number(plugin.commands().len() as f64),
                );
                map.insert(
                    "has_docs".into(),
                    Value::Bool(!plugin.command_docs().is_empty()),
                );
                map.insert("permissions".into(), Value::List(permission_names));
                map.insert(
                    "command_permissions".into(),
                    Value::List(
                        command_permissions
                            .iter()
                            .map(|(command, permission)| {
                                let mut permission_map = HashMap::new();
                                permission_map
                                    .insert("command".into(), Value::String((*command).into()));
                                permission_map.insert(
                                    "permission".into(),
                                    Value::String((*permission).into()),
                                );
                                Value::Object(permission_map)
                            })
                            .collect(),
                    ),
                );
                Value::Object(map)
            })
            .collect();

        Ok(Value::List(plugins))
    }

    pub(crate) fn plugins_reload(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        if !args.is_empty() {
            return Err("plugins.reload expects no arguments".into());
        }

        self.reload_plugins();
        let diagnostics = self.external_plugin_diagnostics();
        let mut map = HashMap::new();
        map.insert("reloaded".into(), Value::Bool(true));
        map.insert(
            "external_loaded".into(),
            Value::Number(diagnostics.loaded.len() as f64),
        );
        map.insert(
            "external_failed".into(),
            Value::Number(diagnostics.failed.len() as f64),
        );
        Ok(Value::Object(map))
    }

    pub(crate) fn plugins_discover(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        if !args.is_empty() {
            return Err("plugins.discover expects no arguments".into());
        }

        let root = self.workspace_root.join(".zen").join("plugins");
        let loaded_sources: HashSet<_> = self
            .plugins
            .iter()
            .filter(|plugin| plugin.kind() == "external")
            .filter_map(|plugin| plugin.source().map(|source| source.to_string()))
            .collect();
        let loaded_names: HashSet<_> = self
            .plugins
            .iter()
            .filter(|plugin| plugin.kind() == "external")
            .map(|plugin| plugin.name().to_string())
            .collect();

        let discovered = discover_external_plugin_manifests(&root)
            .into_iter()
            .map(|plugin| {
                let loaded = plugin
                    .name
                    .as_ref()
                    .map(|name| loaded_names.contains(name))
                    .unwrap_or(false)
                    || loaded_sources.contains(&plugin.source);
                let status = if plugin.error.is_some() {
                    "error"
                } else if loaded {
                    "loaded"
                } else {
                    "available"
                };

                let mut map = HashMap::new();
                map.insert(
                    "name".into(),
                    plugin.name.map(Value::String).unwrap_or(Value::Null),
                );
                map.insert("status".into(), Value::String(status.into()));
                map.insert("loaded".into(), Value::Bool(loaded));
                map.insert(
                    "description".into(),
                    plugin.description.map(Value::String).unwrap_or(Value::Null),
                );
                map.insert(
                    "version".into(),
                    plugin.version.map(Value::String).unwrap_or(Value::Null),
                );
                map.insert(
                    "author".into(),
                    plugin.author.map(Value::String).unwrap_or(Value::Null),
                );
                map.insert(
                    "homepage".into(),
                    plugin.homepage.map(Value::String).unwrap_or(Value::Null),
                );
                map.insert("source".into(), Value::String(plugin.source));
                map.insert(
                    "commands".into(),
                    Value::List(plugin.commands.into_iter().map(Value::String).collect()),
                );
                map.insert(
                    "error".into(),
                    plugin.error.map(Value::String).unwrap_or(Value::Null),
                );
                Value::Object(map)
            })
            .collect();

        Ok(Value::List(discovered))
    }

    pub(crate) fn plugins_load(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("plugins.load expects one path".into());
        }

        let mut args = args;
        let path = value_to_echo_string(self.eval_echo_arg(args.remove(0))?);
        let resolved = self.resolve_workspace_path(&path)?;
        let (plugin, loaded) = load_external_plugin_from_path(&resolved)?;
        let name = plugin.name();

        self.plugins
            .retain(|existing| !(existing.kind() == "external" && existing.name() == name));
        self.plugins.push(plugin);
        record_external_plugin_loaded(loaded.clone());

        let mut map = HashMap::new();
        map.insert("loaded".into(), Value::Bool(true));
        map.insert("name".into(), Value::String(loaded.name));
        map.insert("source".into(), Value::String(loaded.source));
        map.insert(
            "commands".into(),
            Value::List(
                loaded
                    .commands
                    .into_iter()
                    .map(Value::String)
                    .collect::<Vec<_>>(),
            ),
        );
        Ok(Value::Object(map))
    }

    pub(crate) fn plugins_unload(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("plugins.unload expects one plugin name".into());
        }

        let mut args = args;
        let name = value_to_echo_string(self.eval_echo_arg(args.remove(0))?);
        let before = self.plugins.len();
        self.plugins
            .retain(|plugin| !(plugin.kind() == "external" && plugin.name() == name));
        let unloaded = self.plugins.len() != before;
        if unloaded {
            record_external_plugin_unloaded(&name);
        }

        let mut map = HashMap::new();
        map.insert("name".into(), Value::String(name));
        map.insert("unloaded".into(), Value::Bool(unloaded));
        Ok(Value::Object(map))
    }

    pub(crate) fn core_help(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        match args.len() {
            0 => Ok(Value::String(self.help_overview())),
            1 => {
                let mut args = args;
                let command = self.command_name_from_help_arg(args.remove(0))?;
                Ok(Value::String(self.help_for_command(&command)?))
            }
            _ => Err("help expects zero or one command name".into()),
        }
    }

    pub(crate) fn fs_list_builtin(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        self.permissions.check("fs.read")?;
        let path = self.expect_string_arg(args)?;
        self.fs_list(path)
    }

    pub(crate) fn fs_copy_builtin(
        &mut self,
        args: Vec<Expr>,
        input: Value,
    ) -> Result<Value, String> {
        self.permissions.check("fs.write")?;
        match args.len() {
            1 => {
                let dest = self.expect_string_arg(args)?;
                self.fs_copy(input, dest)
            }
            2 => {
                let mut args = args;
                let source = value_to_echo_string(self.eval_echo_arg(args.remove(0))?);
                let destination = value_to_echo_string(self.eval_echo_arg(args.remove(0))?);
                self.fs_copy_file(&source, &destination)
            }
            _ => Err("fs.copy expects <destination> or <source> <destination>".into()),
        }
    }

    pub(crate) fn workspace_root(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        if !args.is_empty() {
            return Err("workspace.root expects no arguments".into());
        }

        Ok(Value::String(
            self.workspace_root.to_string_lossy().into_owned(),
        ))
    }

    pub(crate) fn workspace_cwd(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        if !args.is_empty() {
            return Err("workspace.cwd expects no arguments".into());
        }

        Ok(Value::String(self.cwd.to_string_lossy().into_owned()))
    }

    pub(crate) fn workspace_find(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        self.permissions.check("workspace.read")?;

        if args.len() != 1 {
            return Err("workspace.find expects one pattern".into());
        }

        let mut args = args;
        let pattern = value_to_echo_string(self.eval_echo_arg(args.remove(0))?);
        self.workspace_find_files(&pattern)
    }

    pub(crate) fn workspace_exists(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        self.permissions.check("workspace.read")?;

        if args.len() != 1 {
            return Err("workspace.exists expects one path".into());
        }

        let mut args = args;
        let path = value_to_echo_string(self.eval_echo_arg(args.remove(0))?);
        Ok(Value::Bool(self.resolve_workspace_path(&path)?.exists()))
    }

    pub(crate) fn workspace_read(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        self.permissions.check("workspace.read")?;

        if args.len() != 1 {
            return Err("workspace.read expects one path".into());
        }

        let mut args = args;
        let path = value_to_echo_string(self.eval_echo_arg(args.remove(0))?);
        let resolved = self.resolve_workspace_path(&path)?;
        let metadata =
            fs::metadata(&resolved).map_err(|e| format!("Failed to read '{}': {}", path, e))?;

        if !metadata.is_file() {
            return Err(format!("workspace.read expected file '{}'", path));
        }

        fs::read_to_string(&resolved)
            .map(Value::String)
            .map_err(|e| format!("Failed to read '{}': {}", path, e))
    }

    pub(crate) fn workspace_files(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        self.permissions.check("workspace.read")?;
        let path = self.optional_workspace_path_arg(args, "workspace.files")?;
        self.workspace_entries(path, true)
    }

    pub(crate) fn workspace_dirs(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        self.permissions.check("workspace.read")?;
        let path = self.optional_workspace_path_arg(args, "workspace.dirs")?;
        self.workspace_entries(path, false)
    }

    pub(crate) fn workspace_env(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        self.permissions.check("workspace.env")?;

        if args.len() != 1 {
            return Err("workspace.env expects one variable name".into());
        }

        let mut args = args;
        let name = value_to_echo_string(self.eval_echo_arg(args.remove(0))?);
        Ok(env::var(name).map(Value::String).unwrap_or(Value::Null))
    }

    pub(crate) fn state_save(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        self.permissions.check("state.write")?;

        if !args.is_empty() {
            return Err("state.save expects no arguments".into());
        }

        let state_path = self.state_path();
        let state_dir = state_path
            .parent()
            .ok_or("Could not determine state directory")?;
        fs::create_dir_all(state_dir)
            .map_err(|e| format!("Failed to create state directory: {}", e))?;

        let mut object = JsonMap::new();
        for (name, value) in &self.ctx.vars {
            object.insert(name.clone(), value_to_json(value));
        }

        let text = serde_json::to_string_pretty(&JsonValue::Object(object))
            .map_err(|e| format!("Failed to serialize state: {}", e))?;
        fs::write(&state_path, text).map_err(|e| format!("Failed to write state: {}", e))?;

        let mut result = HashMap::new();
        result.insert(
            "path".into(),
            Value::String(state_path.to_string_lossy().into_owned()),
        );
        result.insert("count".into(), Value::Number(self.ctx.vars.len() as f64));
        Ok(Value::Object(result))
    }

    pub(crate) fn state_load(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        self.permissions.check("state.read")?;

        if !args.is_empty() {
            return Err("state.load expects no arguments".into());
        }

        let state_path = self.state_path();
        let text =
            fs::read_to_string(&state_path).map_err(|e| format!("Failed to read state: {}", e))?;
        let json: JsonValue =
            serde_json::from_str(&text).map_err(|e| format!("Failed to parse state: {}", e))?;
        let JsonValue::Object(object) = json else {
            return Err("State file must contain a JSON object".into());
        };

        let count = object.len();
        for (name, value) in object {
            let value = json_to_value(value);
            self.ctx.vars.insert(name.clone(), value.clone());
            self.env.insert(name, value);
        }

        let mut result = HashMap::new();
        result.insert(
            "path".into(),
            Value::String(state_path.to_string_lossy().into_owned()),
        );
        result.insert("count".into(), Value::Number(count as f64));
        Ok(Value::Object(result))
    }

    pub(crate) fn state_clear(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        self.permissions.check("state.write")?;

        if !args.is_empty() {
            return Err("state.clear expects no arguments".into());
        }

        let state_path = self.state_path();
        let deleted = if state_path.exists() {
            fs::remove_file(&state_path).map_err(|e| format!("Failed to clear state: {}", e))?;
            true
        } else {
            false
        };

        let mut result = HashMap::new();
        result.insert(
            "path".into(),
            Value::String(state_path.to_string_lossy().into_owned()),
        );
        result.insert("deleted".into(), Value::Bool(deleted));
        Ok(Value::Object(result))
    }

    pub(crate) fn state_list(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        if !args.is_empty() {
            return Err("state.list expects no arguments".into());
        }

        let mut names: Vec<_> = self.ctx.vars.keys().cloned().collect();
        names.sort();

        Ok(Value::List(
            names
                .into_iter()
                .map(|name| {
                    let mut entry = HashMap::new();
                    entry.insert("name".into(), Value::String(name.clone()));
                    entry.insert(
                        "value".into(),
                        self.ctx.vars.get(&name).cloned().unwrap_or(Value::Null),
                    );
                    Value::Object(entry)
                })
                .collect(),
        ))
    }

    pub(crate) fn process_exec(&mut self, call: FunctionCall) -> Result<Value, String> {
        self.permissions.check("proc.exec")?;
        let request = self.exec_request_from_call(call)?;
        ProcessEffects.perform(Effect::Process(request), &CapabilityGrant::new("proc.exec"))
    }

    pub(crate) fn external_process_exec(
        &mut self,
        base_command: &str,
        call: &FunctionCall,
    ) -> Result<Value, String> {
        self.permissions.check("proc.exec")?;

        let mut command = base_command.to_string();
        let mut argv = Self::split_external_command_line(base_command)?;
        for arg in &call.args {
            let value = self.eval_echo_arg(arg.clone())?;
            let text = value_to_echo_string(value);
            command.push(' ');
            command.push_str(&Self::shell_arg(&text));
            argv.push(text);
        }

        let (env, secret_values) =
            crate::runtime::plugins::secrets::resolve_env_config(self, call.config.clone())?;
        let request = ExecRequest {
            command,
            argv: Some(argv),
            attempts: 1,
            timeout: None,
            wait_children: false,
            workdir: Some(self.cwd.to_string_lossy().into_owned()),
            env,
            secret_values,
        };
        ProcessEffects.perform(Effect::Process(request), &CapabilityGrant::new("proc.exec"))
    }

    pub(crate) fn process_list_builtin(&mut self) -> Result<Value, String> {
        self.permissions.check("proc.read")?;
        self.process_list()
    }

    pub(crate) fn time_builtin(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String> {
        self.time_value(args, input)
    }

    pub(crate) fn time_builtin_with_mode(
        &mut self,
        mode: &str,
        args: Vec<Expr>,
        input: Value,
    ) -> Result<Value, String> {
        self.time_value_with_mode(mode, args, input)
    }

    pub(crate) fn time_local(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String> {
        self.local_time(args, input)
    }

    pub(crate) fn time_local_with_mode(
        &mut self,
        mode: &str,
        args: Vec<Expr>,
        input: Value,
    ) -> Result<Value, String> {
        self.local_time_with_mode(mode, args, input)
    }

    pub(crate) fn time_freeze(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String> {
        self.freeze_time(args, input)
    }

    pub(crate) fn time_measure(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String> {
        self.measure(args, input)
    }

    pub(crate) fn time_benchmark(
        &mut self,
        args: Vec<Expr>,
        input: Value,
    ) -> Result<Value, String> {
        self.benchmark(args, input)
    }

    pub(crate) fn time_sleep(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String> {
        self.sleep(args, input)
    }

    fn expect_string_arg(&mut self, args: Vec<Expr>) -> Result<String, String> {
        if args.len() != 1 {
            return Err("Expected one string argument".into());
        }
        let mut args = args;
        let expr = args.pop().unwrap();

        let val = Self::eval_expr(&expr, &mut self.ctx)?;
        val.as_string()
            .map(|s| s.to_string())
            .ok_or("Expected string argument".into())
    }

    fn command_name_from_help_arg(&mut self, expr: Expr) -> Result<String, String> {
        match expr {
            Expr::Ident(name) => Ok(name),
            Expr::String(name) => Ok(name),
            Expr::Call(call) if call.args.is_empty() => Ok(call.name.join(".")),
            expr => Ok(value_to_echo_string(self.eval_echo_arg(expr)?)),
        }
    }

    fn help_overview(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "Builtin commands");
        let _ = writeln!(out);

        for plugin in &self.plugins {
            let version = plugin
                .version()
                .map(|version| format!(" v{}", version))
                .unwrap_or_default();
            let _ = writeln!(out, "{}{}:", plugin.name(), version);
            if let Some(description) = plugin.description() {
                let _ = writeln!(out, "  {}", description);
            }
            for command in plugin.commands() {
                let summary = self
                    .command_doc(command)
                    .map(|doc| doc.summary)
                    .unwrap_or("No help available.");
                let permissions = self
                    .command_permissions(command)
                    .map(|permissions| format!(" [{}]", permissions.join(", ")))
                    .unwrap_or_default();
                let _ = writeln!(out, "  {:<18} {}{}", command, summary, permissions);
            }
            let _ = writeln!(out);
        }

        out.push_str("Use: help <command>\n");
        out
    }

    fn help_for_command(&self, command: &str) -> Result<String, String> {
        let Some(doc) = self.command_doc(command) else {
            if self.is_builtin_command(command) {
                return Ok(format!("{}\n\nNo detailed help available yet.\n", command));
            }

            return Err(format!("Unknown command '{}'", command));
        };

        let mut out = String::new();
        let _ = writeln!(out, "{}", doc.command);
        let _ = writeln!(out);
        let _ = writeln!(out, "{}", doc.summary);
        let _ = writeln!(out);
        let _ = writeln!(out, "Usage:");
        let _ = writeln!(out, "  {}", doc.usage);
        let _ = writeln!(out);
        let _ = writeln!(out, "Requires:");
        if let Some(permissions) = self.command_permissions(command) {
            for permission in permissions {
                let _ = writeln!(out, "  {}", permission);
            }
        } else {
            let _ = writeln!(out, "  none");
        }

        if !doc.examples.is_empty() {
            let _ = writeln!(out);
            let _ = writeln!(out, "Examples:");
            for example in doc.examples {
                let _ = writeln!(out, "  {}", example);
            }
        }

        Ok(out)
    }

    fn command_doc(&self, command: &str) -> Option<&CommandDoc> {
        self.plugins
            .iter()
            .flat_map(|plugin| plugin.command_docs())
            .find(|doc| doc.command == command)
    }

    pub(crate) fn command_permissions(&self, command: &str) -> Option<Vec<&'static str>> {
        let permissions: Vec<_> = self
            .plugins
            .iter()
            .flat_map(|plugin| plugin.command_permissions())
            .filter_map(|(doc_command, permission)| {
                (*doc_command == command).then_some(*permission)
            })
            .collect();

        (!permissions.is_empty()).then_some(permissions)
    }

    fn echo(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String> {
        let mut args = args;
        let newline_separator = matches!(args.first(), Some(Expr::String(flag)) if flag == "-n");
        if newline_separator {
            args.remove(0);
        }

        let mode = match args.first() {
            Some(Expr::Ident(mode))
                if matches!(mode.as_str(), "json" | "table" | "success" | "error") =>
            {
                let mode = mode.clone();
                args.remove(0);
                Some(mode)
            }
            _ => None,
        };

        match mode.as_deref() {
            Some("json") => {
                let value = self.echo_target(args, input)?;
                println!("{}", value_to_json(&value));
            }
            Some("table") => {
                let value = self.echo_target(args, input)?;
                print!("{}", Self::value_to_table(&value));
            }
            Some("success") => {
                println!("success: {}", self.echo_text(args, newline_separator)?);
            }
            Some("error") => {
                eprintln!("error: {}", self.echo_text(args, newline_separator)?);
            }
            _ => {
                println!("{}", self.echo_text(args, newline_separator)?);
            }
        }

        Ok(Value::Null)
    }

    fn echo_text(&mut self, args: Vec<Expr>, newline_separator: bool) -> Result<String, String> {
        let mut parts = Vec::new();

        for expr in args {
            let value = self.eval_echo_arg(expr)?;
            parts.push(value_to_echo_string(value));
        }

        Ok(parts.join(if newline_separator { "\n" } else { " " }))
    }

    fn exec_request_from_call(&mut self, call: FunctionCall) -> Result<ExecRequest, String> {
        if call.args.is_empty() {
            return Err("Expected command after exec".into());
        }

        let mut parts = Vec::new();

        for expr in call.args {
            let value = self.eval_echo_arg(expr)?;
            parts.push(value_to_echo_string(value));
        }

        let mut command_parts = Vec::new();
        let mut attempts = 1usize;
        let mut timeout = None;
        let mut wait_children = false;
        let mut workdir = Some(self.cwd.to_string_lossy().into_owned());
        let mut i = 0usize;

        while i < parts.len() {
            match parts[i].as_str() {
                "retry" => {
                    let Some(raw_count) = parts.get(i + 1) else {
                        return Err("Expected number after exec retry".into());
                    };
                    attempts = raw_count
                        .parse::<usize>()
                        .map_err(|_| "Expected numeric retry count".to_string())?
                        .max(1);
                    i += 2;
                }
                "timeout" => {
                    let Some(raw_duration) = parts.get(i + 1) else {
                        return Err("Expected duration after exec timeout".into());
                    };

                    let duration = if let Some(unit) = parts.get(i + 2) {
                        if matches!(unit.as_str(), "ms" | "s" | "m") {
                            i += 3;
                            parse_duration(&format!("{}{}", raw_duration, unit))?
                        } else {
                            i += 2;
                            parse_duration(raw_duration)?
                        }
                    } else {
                        i += 2;
                        parse_duration(raw_duration)?
                    };

                    timeout = Some(duration);
                }
                "wait" => {
                    let Some(mode) = parts.get(i + 1) else {
                        return Err("Expected wait mode after exec wait".into());
                    };

                    match mode.as_str() {
                        "children" => {
                            wait_children = true;
                            i += 2;
                        }
                        _ => return Err(format!("Unsupported exec wait mode '{}'", mode)),
                    }
                }
                "workdir" => {
                    let Some(path) = parts.get(i + 1) else {
                        return Err("Expected path after exec workdir".into());
                    };

                    workdir = Some(self.resolve_fs_path(path).to_string_lossy().into_owned());
                    i += 2;
                }
                _ => {
                    command_parts.push(parts[i].clone());
                    i += 1;
                }
            }
        }

        if command_parts.is_empty() {
            return Err("Expected command after exec".into());
        }

        let command = command_parts
            .iter()
            .map(|part| Self::shell_arg(part))
            .collect::<Vec<_>>()
            .join(" ");

        let (env, secret_values) = crate::runtime::plugins::secrets::resolve_env_config(
            self,
            call.config,
        )?;
        Ok(ExecRequest {
            command,
            argv: Some(command_parts),
            attempts,
            timeout,
            wait_children,
            workdir,
            env,
            secret_values,
        })
    }

    fn cd(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        if args.len() > 1 {
            return Err("cd expects zero or one path".into());
        }

        let path = if let Some(expr) = args.into_iter().next() {
            value_to_echo_string(self.eval_echo_arg(expr)?)
        } else {
            dirs::home_dir()
                .ok_or("Could not determine home directory")?
                .to_string_lossy()
                .into_owned()
        };

        let resolved = self.resolve_fs_path(&path);
        let metadata =
            fs::metadata(&resolved).map_err(|e| format!("Invalid directory '{}': {}", path, e))?;

        if !metadata.is_dir() {
            return Err(format!("Invalid directory '{}': not a directory", path));
        }

        self.cwd = resolved;
        Ok(Value::String(self.cwd.to_string_lossy().into_owned()))
    }

    fn pwd(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        if !args.is_empty() {
            return Err("pwd expects no arguments".into());
        }

        Ok(Value::String(self.cwd.to_string_lossy().into_owned()))
    }

    fn which(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        if args.len() != 1 {
            return Err("which expects one command name".into());
        }

        let mut args = args;
        let command = match args.remove(0) {
            Expr::Ident(name) => name,
            Expr::Call(call) if call.args.is_empty() => call.name.join("."),
            expr => value_to_echo_string(self.eval_echo_arg(expr)?),
        };

        if self.is_builtin_command(&command) {
            return Ok(Value::String(format!("builtin:{}", command)));
        }

        if let Some(path) = Self::find_in_path(&command) {
            return Ok(Value::String(path.to_string_lossy().into_owned()));
        }

        Ok(Value::Null)
    }

    fn clear(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        if !args.is_empty() {
            return Err("clear expects no arguments".into());
        }

        terminal::clear_screen()?;
        Ok(Value::Null)
    }

    fn resolve_fs_path(&self, path: &str) -> PathBuf {
        let path = Path::new(path);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cwd.join(path)
        }
    }

    pub(crate) fn resolve_local_write_path(&self, path: &str) -> Result<PathBuf, String> {
        let path = Path::new(path);
        if path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err("local write paths cannot traverse parent directories".into());
        }

        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cwd.join(path)
        };

        if path.is_absolute() && !resolved.starts_with(&self.workspace_root) {
            return Err("local write paths must stay inside the workspace root".into());
        }

        Ok(resolved)
    }

    pub(crate) fn resolve_workspace_path(&self, path: &str) -> Result<PathBuf, String> {
        let path = Path::new(path);
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.workspace_root.join(path)
        };

        if resolved
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err("workspace paths cannot traverse outside the workspace".into());
        }

        if path.is_absolute() && !resolved.starts_with(&self.workspace_root) {
            return Err("workspace paths must stay inside the workspace root".into());
        }

        Ok(resolved)
    }

    fn state_path(&self) -> PathBuf {
        self.workspace_root.join(".zen").join("state.json")
    }

    fn optional_workspace_path_arg(
        &mut self,
        args: Vec<Expr>,
        command: &str,
    ) -> Result<PathBuf, String> {
        match args.len() {
            0 => self.resolve_workspace_path("."),
            1 => {
                let mut args = args;
                let path = value_to_echo_string(self.eval_echo_arg(args.remove(0))?);
                self.resolve_workspace_path(&path)
            }
            _ => Err(format!("{} expects zero or one path", command)),
        }
    }

    fn discover_workspace_root(start: &Path) -> PathBuf {
        let mut current = start.to_path_buf();

        loop {
            if current.join("Cargo.toml").is_file()
                || current.join(".git").is_dir()
                || current.join("package.json").is_file()
            {
                return current;
            }

            if !current.pop() {
                return start.to_path_buf();
            }
        }
    }

    fn normalize_canonical_path(path: PathBuf) -> PathBuf {
        #[cfg(windows)]
        {
            let text = path.to_string_lossy();
            if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
                return PathBuf::from(format!(r"\\{rest}"));
            }
            if let Some(rest) = text.strip_prefix(r"\\?\") {
                return PathBuf::from(rest);
            }
        }

        path
    }

    fn workspace_find_files(&self, pattern: &str) -> Result<Value, String> {
        let mut entries = Vec::new();
        self.collect_workspace_matches(&self.workspace_root, pattern, &mut entries)?;
        entries.sort_by(|left, right| {
            let left_path = match left {
                Value::Object(map) => map.get("path").and_then(Value::as_string).unwrap_or(""),
                _ => "",
            };
            let right_path = match right {
                Value::Object(map) => map.get("path").and_then(Value::as_string).unwrap_or(""),
                _ => "",
            };
            left_path.cmp(right_path)
        });
        Ok(Value::List(entries))
    }

    fn workspace_entries(&self, path: PathBuf, files: bool) -> Result<Value, String> {
        let metadata =
            fs::metadata(&path).map_err(|e| format!("Failed to read workspace path: {}", e))?;
        if !metadata.is_dir() {
            return Err("workspace.files/workspace.dirs expected a directory".into());
        }

        let mut entries = Vec::new();
        for entry in
            fs::read_dir(&path).map_err(|e| format!("Failed to read workspace path: {}", e))?
        {
            let entry = entry.map_err(|e| e.to_string())?;
            let metadata = entry.metadata().map_err(|e| e.to_string())?;

            if (files && !metadata.is_file()) || (!files && !metadata.is_dir()) {
                continue;
            }

            let path = entry.path();
            let relative = path
                .strip_prefix(&self.workspace_root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let mut map = HashMap::new();
            map.insert("path".into(), Value::String(relative));
            map.insert(
                "name".into(),
                Value::String(entry.file_name().to_string_lossy().into_owned()),
            );
            map.insert("size".into(), Value::Number(metadata.len() as f64));
            entries.push(Value::Object(map));
        }

        entries.sort_by(|left, right| {
            let left_path = match left {
                Value::Object(map) => map.get("path").and_then(Value::as_string).unwrap_or(""),
                _ => "",
            };
            let right_path = match right {
                Value::Object(map) => map.get("path").and_then(Value::as_string).unwrap_or(""),
                _ => "",
            };
            left_path.cmp(right_path)
        });

        Ok(Value::List(entries))
    }

    fn collect_workspace_matches(
        &self,
        dir: &Path,
        pattern: &str,
        entries: &mut Vec<Value>,
    ) -> Result<(), String> {
        for entry in fs::read_dir(dir).map_err(|e| format!("Failed to read workspace: {}", e))? {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            let file_name = entry.file_name().to_string_lossy().into_owned();

            if path.is_dir() {
                if matches!(file_name.as_str(), ".git" | "target") {
                    continue;
                }
                self.collect_workspace_matches(&path, pattern, entries)?;
                continue;
            }

            if !path.is_file() {
                continue;
            }

            let relative = path
                .strip_prefix(&self.workspace_root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");

            if !Self::wildcard_matches(pattern, &relative)
                && !Self::wildcard_matches(pattern, &file_name)
            {
                continue;
            }

            let metadata = entry.metadata().map_err(|e| e.to_string())?;
            let mut map = HashMap::new();
            map.insert("path".into(), Value::String(relative));
            map.insert("size".into(), Value::Number(metadata.len() as f64));
            entries.push(Value::Object(map));
        }

        Ok(())
    }

    fn wildcard_matches(pattern: &str, text: &str) -> bool {
        let pattern = pattern.as_bytes();
        let text = text.as_bytes();
        let mut pattern_index = 0;
        let mut text_index = 0;
        let mut star_index = None;
        let mut text_after_star = 0;

        while text_index < text.len() {
            if pattern_index < pattern.len()
                && (pattern[pattern_index] == b'?' || pattern[pattern_index] == text[text_index])
            {
                pattern_index += 1;
                text_index += 1;
            } else if pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
                star_index = Some(pattern_index);
                pattern_index += 1;
                text_after_star = text_index;
            } else if let Some(star) = star_index {
                pattern_index = star + 1;
                text_after_star += 1;
                text_index = text_after_star;
            } else {
                return false;
            }
        }

        while pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
            pattern_index += 1;
        }

        pattern_index == pattern.len()
    }

    fn is_builtin_command(&self, command: &str) -> bool {
        self.plugins
            .iter()
            .any(|plugin| plugin.commands().contains(&command))
    }

    fn find_in_path(command: &str) -> Option<PathBuf> {
        let command_path = Path::new(command);
        if command_path.components().count() > 1 {
            return command_path.is_file().then(|| command_path.to_path_buf());
        }

        let path_exts = if cfg!(windows) {
            let mut exts = env::var_os("PATHEXT")
                .map(|value| {
                    env::split_paths(&value)
                        .map(|path| path.to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                })
                .filter(|exts| !exts.is_empty())
                .unwrap_or_else(|| vec![".EXE".into(), ".CMD".into(), ".BAT".into()]);
            exts.insert(0, String::new());
            exts
        } else {
            vec!["".into()]
        };

        let path_var = env::var_os("PATH")?;
        for dir in env::split_paths(&path_var) {
            for ext in &path_exts {
                let candidate = dir.join(format!("{}{}", command, ext));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }

        None
    }

    fn echo_target(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String> {
        if !matches!(input, Value::Null) {
            return Ok(input);
        }

        if let Some(expr) = args.into_iter().next() {
            return self.eval_echo_arg(expr);
        }

        Ok(Value::Null)
    }

    fn eval_echo_arg(&mut self, expr: Expr) -> Result<Value, String> {
        match expr {
            Expr::Ident(name) => Ok(self
                .ctx
                .vars
                .get(&name)
                .or_else(|| self.env.get(&name))
                .cloned()
                .unwrap_or(Value::String(name))),
            Expr::Variable(name) => self
                .ctx
                .vars
                .get(&name)
                .or_else(|| self.env.get(&name))
                .cloned()
                .ok_or_else(|| format!("Undefined variable '${}'", name)),
            Expr::Call(call) if call.args.is_empty() && call.name.len() > 1 => self
                .resolve_path(&call.name)
                .unwrap_or_else(|| Ok(Value::String(call.name.join(".")))),
            other => Self::eval_expr(&other, &mut self.ctx),
        }
    }

    fn resolve_path(&self, parts: &[String]) -> Option<Result<Value, String>> {
        let first = parts.first()?;
        let mut value = self
            .ctx
            .vars
            .get(first)
            .or_else(|| self.env.get(first))?
            .clone();

        for part in &parts[1..] {
            match value {
                Value::Object(map) => {
                    value = map.get(part).cloned().unwrap_or(Value::Null);
                }
                _ => return Some(Err(format!("Cannot access field '{}' on non-object", part))),
            }
        }

        Some(Ok(value))
    }

    fn shell_arg(value: &str) -> String {
        if !value.is_empty()
            && value.chars().all(|ch| {
                ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | '\\' | ':')
            })
        {
            return value.into();
        }

        format!("\"{}\"", value.replace('"', "\\\""))
    }

    fn split_external_command_line(command: &str) -> Result<Vec<String>, String> {
        let mut args = Vec::new();
        let mut current = String::new();
        let mut quote = None;
        let mut chars = command.chars().peekable();

        while let Some(ch) = chars.next() {
            match quote {
                Some(q) if ch == q => quote = None,
                Some(_) if ch == '\\' && matches!(chars.peek(), Some('"') | Some('\'')) => {
                    current.push(chars.next().expect("peeked character exists"));
                }
                Some(_) => current.push(ch),
                None if ch == '"' || ch == '\'' => quote = Some(ch),
                None if ch.is_whitespace() => {
                    if !current.is_empty() {
                        args.push(std::mem::take(&mut current));
                    }
                }
                None => current.push(ch),
            }
        }

        if let Some(q) = quote {
            return Err(format!(
                "External command has unterminated {} quote: {}",
                q, command
            ));
        }

        if !current.is_empty() {
            args.push(current);
        }

        if args.is_empty() {
            return Err("External command is empty".into());
        }

        Ok(args)
    }

    pub(crate) fn workflow_run(
        &mut self,
        mut args: Vec<Expr>,
        input: Value,
    ) -> Result<Value, String> {
        if args.len() > 1 {
            return Err("workflow.run expects one workflow object".into());
        }

        let value = if let Some(expr) = args.pop() {
            self.eval_echo_arg(expr)?
        } else {
            input
        };
        WorkflowEngine::new(self).run(value)
    }

    pub fn workflow_run_persisted(&mut self, value: Value, source: &str) -> Result<Value, String> {
        WorkflowEngine::new(self).run_persisted(value, source)
    }

    pub fn workflow_resume_persisted(
        &mut self,
        value: Value,
        source: &str,
        run_id: &str,
    ) -> Result<Value, String> {
        WorkflowEngine::new(self).resume_persisted(value, source, run_id)
    }

    pub fn workflow_runtime_db_path(&self) -> PathBuf {
        workflow::runtime_db_path(&self.workspace_root)
    }

    pub fn workspace_root_path(&self) -> &Path {
        &self.workspace_root
    }

    pub fn cwd_path(&self) -> &Path {
        &self.cwd
    }

    pub fn workflow_validate(value: Value) -> Result<(), String> {
        workflow::validate(value)
    }

    fn value_to_table(value: &Value) -> String {
        match value {
            Value::List(items) if items.is_empty() => "(empty)\n".into(),
            Value::List(items) => Self::list_to_table(items),
            Value::Object(map) => {
                let mut out = String::new();
                for (key, value) in map {
                    let _ = writeln!(
                        out,
                        "{}: {}",
                        key,
                        value_to_echo_string(value.clone())
                    );
                }
                out
            }
            other => format!("{}\n", value_to_echo_string(other.clone())),
        }
    }

    fn parse_value(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String> {
        let format = match args.first() {
            Some(Expr::Ident(format)) => format.as_str(),
            Some(Expr::String(format)) => format.as_str(),
            _ => return Err("Expected parse format, e.g. parse json".into()),
        };

        match format {
            "json" => self.parse_json(input),
            _ => Err(format!("Unsupported parse format '{}'", format)),
        }
    }

    fn measure(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String> {
        let call = self.call_from_measure_args(args)?;
        let started_at = self.now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let started = Instant::now();
        let result = self.eval_call_with_input(call, input)?;
        let duration = started.elapsed();
        let ended_at = self.now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

        let mut map = HashMap::new();
        map.insert("success".into(), Value::Bool(true));
        map.insert(
            "duration_ms".into(),
            Value::Number(duration.as_secs_f64() * 1000.0),
        );
        map.insert("started_at".into(), Value::String(started_at));
        map.insert("ended_at".into(), Value::String(ended_at));
        map.insert("result".into(), result);

        Ok(Value::Object(map))
    }

    fn call_from_measure_args(&mut self, args: Vec<Expr>) -> Result<FunctionCall, String> {
        let mut args = args;
        let Some(first) = args.first().cloned() else {
            return Err("measure expects a function call, e.g. measure time.now".into());
        };
        args.remove(0);

        match first {
            Expr::Call(mut call) => {
                call.args.extend(args);
                Ok(call)
            }
            Expr::Ident(name) => Ok(FunctionCall {
                name: vec![name],
                args,
                config: None,
            }),
            _ => Err("measure expects a function call, e.g. measure time.now".into()),
        }
    }

    fn benchmark(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String> {
        let (runs, call) = self.benchmark_request_from_args(args)?;
        let mut durations = Vec::with_capacity(runs);
        let mut failures = 0usize;
        let mut last_result = Value::Null;

        for _ in 0..runs {
            let started = Instant::now();
            match self.eval_call_with_input(call.clone(), input.clone()) {
                Ok(result) => {
                    durations.push(started.elapsed().as_secs_f64() * 1000.0);
                    last_result = result;
                }
                Err(_) => {
                    durations.push(started.elapsed().as_secs_f64() * 1000.0);
                    failures += 1;
                }
            }
        }

        durations.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let total: f64 = durations.iter().sum();
        let avg = if durations.is_empty() {
            0.0
        } else {
            total / durations.len() as f64
        };
        let median = if durations.is_empty() {
            0.0
        } else if durations.len() % 2 == 1 {
            durations[durations.len() / 2]
        } else {
            let upper = durations.len() / 2;
            (durations[upper - 1] + durations[upper]) / 2.0
        };

        let mut map = HashMap::new();
        map.insert("runs".into(), Value::Number(runs as f64));
        map.insert("failures".into(), Value::Number(failures as f64));
        map.insert("success".into(), Value::Bool(failures == 0));
        map.insert(
            "min_ms".into(),
            Value::Number(*durations.first().unwrap_or(&0.0)),
        );
        map.insert("avg_ms".into(), Value::Number(avg));
        map.insert("median_ms".into(), Value::Number(median));
        map.insert(
            "max_ms".into(),
            Value::Number(*durations.last().unwrap_or(&0.0)),
        );
        map.insert("last_result".into(), last_result);

        Ok(Value::Object(map))
    }

    fn benchmark_request_from_args(
        &mut self,
        args: Vec<Expr>,
    ) -> Result<(usize, FunctionCall), String> {
        let mut args = args;
        let Some(first) = args.first().cloned() else {
            return Err(
                "benchmark expects runs and a function call, e.g. benchmark 10 sleep 20ms".into(),
            );
        };
        args.remove(0);

        let runs_value = self.eval_echo_arg(first)?;
        let runs = match runs_value {
            Value::Number(n) if n >= 1.0 => n as usize,
            Value::String(s) => s
                .parse::<usize>()
                .map_err(|_| "benchmark runs must be a positive number".to_string())?,
            _ => return Err("benchmark runs must be a positive number".into()),
        };

        if runs == 0 {
            return Err("benchmark runs must be a positive number".into());
        }

        Ok((runs, self.call_from_measure_args(args)?))
    }

    fn sleep(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String> {
        let parts = self.eval_sleep_parts(args)?;
        let duration = self.sleep_duration_from_parts(&parts)?;
        if !duration.is_zero() {
            thread::sleep(duration);
        }
        Ok(input)
    }

    fn eval_sleep_parts(&mut self, args: Vec<Expr>) -> Result<Vec<String>, String> {
        if args.is_empty() {
            return Err("sleep expects a duration, e.g. sleep 500ms".into());
        }

        let mut parts = Vec::new();
        for arg in args {
            parts.push(value_to_echo_string(self.eval_echo_arg(arg)?));
        }
        Ok(parts)
    }

    fn sleep_duration_from_parts(&self, parts: &[String]) -> Result<Duration, String> {
        let mut index = 0usize;
        let mut duration = Duration::ZERO;

        if parts.first().map(String::as_str) == Some("until") {
            let (target, consumed) = self.parse_sleep_until(parts, 1)?;
            let now = Utc::now();
            duration = if target <= now {
                Duration::ZERO
            } else {
                (target - now)
                    .to_std()
                    .map_err(|_| "Invalid sleep until duration".to_string())?
            };
            index = consumed;
        } else if parts.first().map(String::as_str) != Some("jitter") {
            let (base, consumed) = Self::parse_sleep_duration_at(parts, 0)?;
            duration = base;
            index = consumed;
        }

        if parts.get(index).map(String::as_str) == Some("jitter") {
            let (jitter, consumed) = Self::parse_sleep_duration_at(parts, index + 1)?;
            duration += Self::jitter_duration(jitter);
            index = consumed;
        }

        if index != parts.len() {
            return Err(format!("Unexpected sleep argument '{}'", parts[index]));
        }

        Ok(duration)
    }

    fn parse_sleep_until(
        &self,
        parts: &[String],
        index: usize,
    ) -> Result<(DateTime<Utc>, usize), String> {
        let raw = parts
            .get(index)
            .ok_or_else(|| "sleep until expects a timestamp or date".to_string())?;
        let target = parse_time_reference(raw, self.now())?;
        Ok((target, index + 1))
    }

    fn parse_sleep_duration_at(
        parts: &[String],
        index: usize,
    ) -> Result<(Duration, usize), String> {
        let first = parts
            .get(index)
            .ok_or_else(|| "sleep expects a duration, e.g. sleep 500ms".to_string())?;

        let raw = match parts.get(index + 1) {
            Some(unit) if matches!(unit.as_str(), "ms" | "s" | "m") => {
                format!("{}{}", first, unit)
            }
            _ => first.clone(),
        };

        let consumed = if parts
            .get(index + 1)
            .is_some_and(|unit| matches!(unit.as_str(), "ms" | "s" | "m"))
        {
            index + 2
        } else {
            index + 1
        };

        let duration =
            parse_duration(&raw).map_err(|_| format!("Invalid sleep duration '{}'", raw))?;
        Ok((duration, consumed))
    }

    fn jitter_duration(max: Duration) -> Duration {
        if max.is_zero() {
            return Duration::ZERO;
        }

        let max_nanos = max.as_nanos();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let jitter_nanos = nanos % (max_nanos + 1);
        Duration::from_nanos(jitter_nanos.min(u64::MAX as u128) as u64)
    }

    fn now(&self) -> DateTime<Utc> {
        self.mocked_time.unwrap_or_else(Utc::now)
    }

    fn time_value(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String> {
        let mut args = args;
        let mode = match args.first() {
            Some(Expr::Ident(mode)) | Some(Expr::String(mode)) => {
                let mode = mode.clone();
                args.remove(0);
                mode
            }
            _ => "now".into(),
        };

        match mode.as_str() {
            "now" | "timestamp" => Ok(Value::String(
                self.now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            )),
            "unix" => Ok(Value::Number(self.now().timestamp() as f64)),
            "millis" => Ok(Value::Number(self.now().timestamp_millis() as f64)),
            "local" => self.local_time(args, input),
            "format" => {
                let pattern = match args.first() {
                    Some(expr) => value_to_echo_string(self.eval_echo_arg(expr.clone())?),
                    None => "%Y-%m-%dT%H:%M:%SZ".into(),
                };

                match input {
                    Value::Null => Ok(Value::String(
                        self.now()
                            .with_timezone(&Local)
                            .format(&pattern)
                            .to_string(),
                    )),
                    value => Ok(Value::String(Self::format_local_time_value(
                        value, &pattern,
                    )?)),
                }
            }
            "stamp" => {
                let field = match args.first() {
                    Some(expr) => value_to_echo_string(self.eval_echo_arg(expr.clone())?),
                    None => "timestamp".into(),
                };
                self.stamp_value(input, field)
            }
            "since" => self.time_difference(args, input, true),
            "until" => self.time_difference(args, input, false),
            "parse" => self.parse_time_value(args, input),
            "mock" | "freeze" => self.freeze_time(args, input),
            _ => Err(format!("Unsupported time mode '{}'", mode)),
        }
    }

    fn parse_time_value(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String> {
        let raw = match args.first() {
            Some(expr) => value_to_echo_string(self.eval_echo_arg(expr.clone())?),
            None => match input {
                Value::String(s) => s,
                Value::Number(n) => n.to_string(),
                Value::Null => return Err("time.parse expects a time phrase".into()),
                _ => return Err("time.parse expects string or numeric input".into()),
            },
        };

        Ok(Value::String(
            parse_time_reference(&raw, self.now())?
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string(),
        ))
    }

    fn time_difference(
        &mut self,
        args: Vec<Expr>,
        input: Value,
        since: bool,
    ) -> Result<Value, String> {
        let raw = match args.first() {
            Some(expr) => value_to_echo_string(self.eval_echo_arg(expr.clone())?),
            None => match input {
                Value::String(s) => s,
                Value::Number(n) => n.to_string(),
                Value::Null => {
                    let mode = if since { "since" } else { "until" };
                    return Err(format!("time.{} expects a timestamp or date", mode));
                }
                _ => {
                    let mode = if since { "since" } else { "until" };
                    return Err(format!("time.{} expects string or numeric input", mode));
                }
            },
        };

        let target = parse_time_reference(&raw, self.now())?;
        let seconds = if since {
            self.now().timestamp() - target.timestamp()
        } else {
            target.timestamp() - self.now().timestamp()
        };

        Ok(duration_summary(seconds))
    }

    fn time_value_with_mode(
        &mut self,
        mode: &str,
        args: Vec<Expr>,
        input: Value,
    ) -> Result<Value, String> {
        let mut time_args = Vec::with_capacity(args.len() + 1);
        time_args.push(Expr::Ident(mode.into()));
        time_args.extend(args);
        self.time_value(time_args, input)
    }

    fn local_time(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String> {
        let mut args = args;
        let mode = match args.first() {
            Some(Expr::Ident(mode)) | Some(Expr::String(mode))
                if matches!(mode.as_str(), "now" | "format" | "stamp") =>
            {
                let mode = mode.clone();
                args.remove(0);
                mode
            }
            _ => "now".into(),
        };

        match mode.as_str() {
            "now" => Ok(Value::String(
                self.now()
                    .with_timezone(&Local)
                    .format("%Y-%m-%dT%H:%M:%S%:z")
                    .to_string(),
            )),
            "format" => {
                let pattern = match args.first() {
                    Some(expr) => value_to_echo_string(self.eval_echo_arg(expr.clone())?),
                    None => "%Y-%m-%dT%H:%M:%S%:z".into(),
                };

                match input {
                    Value::Null => Ok(Value::String(
                        self.now()
                            .with_timezone(&Local)
                            .format(&pattern)
                            .to_string(),
                    )),
                    value => Ok(Value::String(Self::format_local_time_value(
                        value, &pattern,
                    )?)),
                }
            }
            "stamp" => {
                let field = match args.first() {
                    Some(expr) => value_to_echo_string(self.eval_echo_arg(expr.clone())?),
                    None => "timestamp".into(),
                };
                self.stamp_local_value(input, field)
            }
            _ => Err(format!("Unsupported local time mode '{}'", mode)),
        }
    }

    fn local_time_with_mode(
        &mut self,
        mode: &str,
        args: Vec<Expr>,
        input: Value,
    ) -> Result<Value, String> {
        let mut local_args = Vec::with_capacity(args.len() + 1);
        local_args.push(Expr::Ident(mode.into()));
        local_args.extend(args);
        self.local_time(local_args, input)
    }

    fn freeze_time(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String> {
        let raw = match args.first() {
            Some(expr) => value_to_echo_string(self.eval_echo_arg(expr.clone())?),
            None => match input {
                Value::String(s) => s,
                Value::Null => {
                    return Err("time.freeze expects an RFC3339 timestamp or 'clear'".into())
                }
                _ => return Err("time.freeze expects string input".into()),
            },
        };

        if matches!(raw.as_str(), "clear" | "reset" | "off") {
            self.mocked_time = None;
            return Ok(Value::String("cleared".into()));
        }

        let dt = DateTime::parse_from_rfc3339(&raw)
            .map_err(|e| format!("Expected RFC3339 timestamp string: {}", e))?
            .with_timezone(&Utc);

        self.mocked_time = Some(dt);
        Ok(Value::String(dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()))
    }

    fn stamp_value(&self, input: Value, field: String) -> Result<Value, String> {
        let stamp = Value::String(self.now().format("%Y-%m-%dT%H:%M:%SZ").to_string());

        match input {
            Value::Null => Ok(stamp),
            Value::Object(mut map) => {
                map.insert(field, stamp);
                Ok(Value::Object(map))
            }
            Value::List(items) => {
                let stamped = items
                    .into_iter()
                    .map(|item| {
                        if let Value::Object(mut map) = item {
                            map.insert(field.clone(), stamp.clone());
                            Value::Object(map)
                        } else {
                            item
                        }
                    })
                    .collect();
                Ok(Value::List(stamped))
            }
            other => Ok(other),
        }
    }

    fn stamp_local_value(&self, input: Value, field: String) -> Result<Value, String> {
        let stamp = Value::String(
            self.now()
                .with_timezone(&Local)
                .format("%Y-%m-%dT%H:%M:%S%:z")
                .to_string(),
        );

        match input {
            Value::Null => Ok(stamp),
            Value::Object(mut map) => {
                map.insert(field, stamp);
                Ok(Value::Object(map))
            }
            Value::List(items) => {
                let stamped = items
                    .into_iter()
                    .map(|item| {
                        if let Value::Object(mut map) = item {
                            map.insert(field.clone(), stamp.clone());
                            Value::Object(map)
                        } else {
                            item
                        }
                    })
                    .collect();
                Ok(Value::List(stamped))
            }
            other => Ok(other),
        }
    }

    fn format_local_time_value(value: Value, pattern: &str) -> Result<String, String> {
        match value {
            Value::Number(n) => {
                let seconds = n as i64;
                let dt = Utc
                    .timestamp_opt(seconds, 0)
                    .single()
                    .ok_or_else(|| format!("Invalid epoch seconds '{}'", seconds))?;
                Ok(dt.with_timezone(&Local).format(pattern).to_string())
            }
            Value::String(s) => {
                let dt = DateTime::parse_from_rfc3339(&s)
                    .map_err(|e| format!("Expected RFC3339 timestamp string: {}", e))?;
                Ok(dt.with_timezone(&Local).format(pattern).to_string())
            }
            _ => Err("time local format expects a timestamp string or epoch seconds".into()),
        }
    }

    fn parse_json(&self, input: Value) -> Result<Value, String> {
        let text = match input {
            Value::String(s) => s,
            Value::Object(map) => match map.get("stdout") {
                Some(Value::String(s)) => s.clone(),
                _ => {
                    return Err("parse json expects string input or exec output with stdout".into())
                }
            },
            _ => return Err("parse json expects string input or exec output".into()),
        };

        let json: JsonValue =
            serde_json::from_str(&text).map_err(|e| format!("Failed to parse JSON: {}", e))?;
        Ok(json_to_value(json))
    }

    fn list_to_table(items: &[Value]) -> String {
        let Some(Value::Object(first)) = items.first() else {
            let mut out = String::new();
            for item in items {
                let _ = writeln!(out, "{}", value_to_echo_string(item.clone()));
            }
            return out;
        };

        let mut headers: Vec<String> = first.keys().cloned().collect();
        headers.sort();

        let mut widths: HashMap<String, usize> = headers
            .iter()
            .map(|header| (header.clone(), header.len()))
            .collect();

        for item in items {
            if let Value::Object(map) = item {
                for header in &headers {
                    let value = map
                        .get(header)
                        .map(|value| value_to_echo_string(value.clone()))
                        .unwrap_or_default();
                    let width = widths.get_mut(header).unwrap();
                    *width = (*width).max(value.len());
                }
            }
        }

        let mut out = String::new();
        for header in &headers {
            let _ = write!(out, "{:<width$}  ", header, width = widths[header]);
        }
        out.push('\n');

        for header in &headers {
            let _ = write!(out, "{:-<width$}  ", "", width = widths[header]);
        }
        out.push('\n');

        for item in items {
            if let Value::Object(map) = item {
                for header in &headers {
                    let value = map
                        .get(header)
                        .map(|value| value_to_echo_string(value.clone()))
                        .unwrap_or_default();
                    let _ = write!(out, "{:<width$}  ", value, width = widths[header]);
                }
                out.push('\n');
            }
        }

        out
    }

    fn fs_list(&self, path: String) -> Result<Value, String> {
        let resolved = self.resolve_fs_path(&path);
        let entries = fs::read_dir(&resolved).map_err(|e| format!("Failed to read dir: {}", e))?;

        let mut list = Vec::new();

        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let metadata = entry.metadata().map_err(|e| e.to_string())?;

            let mut map = HashMap::new();
            map.insert(
                "name".into(),
                Value::String(entry.file_name().to_string_lossy().into()),
            );
            map.insert("size".into(), Value::Number(metadata.len() as f64));

            list.push(Value::Object(map));
        }

        Ok(Value::List(list))
    }

    fn process_list(&self) -> Result<Value, String> {
        let mut system = System::new_all();
        system.refresh_all();

        let mut list = Vec::new();

        for (pid, process) in system.processes() {
            let mut map = HashMap::new();

            map.insert("pid".into(), Value::Number(pid.as_u32() as f64));

            map.insert("name".into(), Value::String(process.name().to_string()));

            map.insert("cpu".into(), Value::Number(process.cpu_usage() as f64));

            map.insert("memory".into(), Value::Number(process.memory() as f64));

            list.push(Value::Object(map));
        }

        Ok(Value::List(list))
    }

    fn fs_copy(&self, input: Value, dest: String) -> Result<Value, String> {
        if let Value::List(items) = input {
            let dest_dir = self.resolve_local_write_path(&dest)?;
            fs::create_dir_all(&dest_dir)
                .map_err(|error| format!("Failed to create '{}': {}", dest_dir.display(), error))?;
            let mut copied = Vec::new();
            for item in items {
                if let Value::Object(map) = item {
                    if let Some(Value::String(name)) = map.get("name") {
                        let src_path = self.resolve_workspace_path(name)?;
                        let dest_path = dest_dir.join(name);
                        let bytes = fs::copy(&src_path, &dest_path).map_err(|error| {
                            format!(
                                "Failed to copy '{}' to '{}': {}",
                                src_path.display(),
                                dest_path.display(),
                                error
                            )
                        })?;
                        let mut entry = HashMap::new();
                        entry.insert(
                            "source".into(),
                            Value::String(src_path.display().to_string()),
                        );
                        entry.insert(
                            "destination".into(),
                            Value::String(dest_path.display().to_string()),
                        );
                        entry.insert("bytes".into(), Value::Number(bytes as f64));
                        copied.push(Value::Object(entry));
                    }
                }
            }

            let mut map = HashMap::new();
            map.insert("success".into(), Value::Bool(true));
            map.insert("copied".into(), Value::List(copied));
            Ok(Value::Object(map))
        } else {
            Err("fs.copy expects list input".into())
        }
    }

    fn fs_copy_file(&self, source: &str, destination: &str) -> Result<Value, String> {
        let source_path = self.resolve_workspace_path(source)?;
        let destination_path = self.resolve_local_write_path(destination)?;
        if let Some(parent) = destination_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("Failed to create '{}': {}", parent.display(), error))?;
        }

        let bytes = fs::copy(&source_path, &destination_path).map_err(|error| {
            format!(
                "Failed to copy '{}' to '{}': {}",
                source_path.display(),
                destination_path.display(),
                error
            )
        })?;

        let mut map = HashMap::new();
        map.insert("success".into(), Value::Bool(true));
        map.insert(
            "source".into(),
            Value::String(source_path.display().to_string()),
        );
        map.insert(
            "destination".into(),
            Value::String(destination_path.display().to_string()),
        );
        map.insert("bytes".into(), Value::Number(bytes as f64));
        Ok(Value::Object(map))
    }

    fn cmp(l: Value, r: Value, f: fn(f64, f64) -> bool) -> Result<Value, String> {
        match (l, r) {
            (Value::Number(a), Value::Number(b)) => Ok(Value::Bool(f(a, b))),
            _ => Err("Comparison requires numbers".into()),
        }
    }

}

impl ScriptRunner for Executor {
    fn run_capture(&mut self, source: &str) -> Result<Value, String> {
        let src = format!("{}\n", source);
        let tokens = Lexer::new(&src).tokenize()?;
        let mut parser = Parser::new(tokens, &src);
        let program = parser.parse_program()?;
        self.execute_capture(program)
    }
}

impl SecretStore for Executor {
    fn read_secret(&self, name: &str) -> Result<Option<String>, String> {
        crate::runtime::plugins::secrets::read_secret(name)
    }
}

impl WorkflowHost for Executor {
    fn check_permission(&self, permission: &str) -> Result<(), String> {
        Executor::check_permission(self, permission)
    }

    fn resolve_workspace_path(&self, path: &str) -> Result<PathBuf, String> {
        Executor::resolve_workspace_path(self, path)
    }

    fn resolve_local_write_path(&self, path: &str) -> Result<PathBuf, String> {
        Executor::resolve_local_write_path(self, path)
    }

    fn workspace_root_path(&self) -> &Path {
        Executor::workspace_root_path(self)
    }

    fn cwd_path(&self) -> &Path {
        Executor::cwd_path(self)
    }
}

impl PluginHost for Executor {
    fn check_permission(&self, permission: &str) -> Result<(), String> {
        Executor::check_permission(self, permission)
    }

    fn plugin_arg_value(&mut self, expr: Expr) -> Result<Value, String> {
        Executor::plugin_arg_value(self, expr)
    }

    fn resolve_workspace_path(&self, path: &str) -> Result<PathBuf, String> {
        Executor::resolve_workspace_path(self, path)
    }

    fn resolve_local_write_path(&self, path: &str) -> Result<PathBuf, String> {
        Executor::resolve_local_write_path(self, path)
    }

    fn core_echo(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String> {
        Executor::core_echo(self, args, input)
    }

    fn core_parse(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String> {
        Executor::core_parse(self, args, input)
    }

    fn core_which(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        Executor::core_which(self, args)
    }

    fn core_clear(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        Executor::core_clear(self, args)
    }

    fn core_cd(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        Executor::core_cd(self, args)
    }

    fn core_pwd(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        Executor::core_pwd(self, args)
    }

    fn core_help(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        Executor::core_help(self, args)
    }

    fn plugins_reload(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        Executor::plugins_reload(self, args)
    }

    fn plugins_discover(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        Executor::plugins_discover(self, args)
    }

    fn plugins_load(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        Executor::plugins_load(self, args)
    }

    fn plugins_unload(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        Executor::plugins_unload(self, args)
    }

    fn process_exec(&mut self, call: FunctionCall) -> Result<Value, String> {
        Executor::process_exec(self, call)
    }

    fn external_process_exec(
        &mut self,
        base_command: &str,
        call: &FunctionCall,
    ) -> Result<Value, String> {
        Executor::external_process_exec(self, base_command, call)
    }

    fn workflow_run(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String> {
        Executor::workflow_run(self, args, input)
    }

    fn workspace_root(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        Executor::workspace_root(self, args)
    }

    fn workspace_cwd(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        Executor::workspace_cwd(self, args)
    }

    fn workspace_find(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        Executor::workspace_find(self, args)
    }

    fn workspace_exists(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        Executor::workspace_exists(self, args)
    }

    fn workspace_read(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        Executor::workspace_read(self, args)
    }

    fn workspace_files(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        Executor::workspace_files(self, args)
    }

    fn workspace_dirs(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        Executor::workspace_dirs(self, args)
    }

    fn workspace_env(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        Executor::workspace_env(self, args)
    }

    fn state_save(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        Executor::state_save(self, args)
    }

    fn state_load(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        Executor::state_load(self, args)
    }

    fn state_clear(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        Executor::state_clear(self, args)
    }

    fn state_list(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        Executor::state_list(self, args)
    }

    fn time_builtin(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String> {
        Executor::time_builtin(self, args, input)
    }

    fn time_builtin_with_mode(
        &mut self,
        mode: &str,
        args: Vec<Expr>,
        input: Value,
    ) -> Result<Value, String> {
        Executor::time_builtin_with_mode(self, mode, args, input)
    }

    fn time_local(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String> {
        Executor::time_local(self, args, input)
    }

    fn time_local_with_mode(
        &mut self,
        mode: &str,
        args: Vec<Expr>,
        input: Value,
    ) -> Result<Value, String> {
        Executor::time_local_with_mode(self, mode, args, input)
    }

    fn time_freeze(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String> {
        Executor::time_freeze(self, args, input)
    }

    fn time_measure(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String> {
        Executor::time_measure(self, args, input)
    }

    fn time_benchmark(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String> {
        Executor::time_benchmark(self, args, input)
    }

    fn time_sleep(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String> {
        Executor::time_sleep(self, args, input)
    }

    fn fs_list_builtin(&mut self, args: Vec<Expr>) -> Result<Value, String> {
        Executor::fs_list_builtin(self, args)
    }

    fn fs_copy_builtin(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String> {
        Executor::fs_copy_builtin(self, args, input)
    }

    fn process_list_builtin(&mut self) -> Result<Value, String> {
        Executor::process_list_builtin(self)
    }

    fn plugins_list(&self) -> Result<Value, String> {
        Executor::plugins_list(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    #[test]
    fn fresh_executor_auto_grants_time_and_rand() {
        let executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        assert!(executor.permissions.granted().contains("time"));
        assert!(executor.permissions.granted().contains("rand"));
    }

    struct TestPlugin;

    impl ZenPlugin for TestPlugin {
        fn name(&self) -> &'static str {
            "test"
        }

        fn commands(&self) -> &'static [&'static str] {
            &["test.ping"]
        }

        fn call(
            &self,
            _executor: &mut dyn PluginHost,
            call: &FunctionCall,
            _input: &Value,
        ) -> Result<PluginResult, String> {
            match call.name.join(".").as_str() {
                "test.ping" => Ok(PluginResult::handled(Value::String("pong".into()))),
                _ => Ok(PluginResult::unhandled()),
            }
        }
    }

    fn executor() -> Executor {
        Executor::new_with_permissions(PermissionSet::new(&Vec::new()))
    }

    fn executor_with_exec_permission() -> Executor {
        Executor::new_with_permissions(PermissionSet::new(&vec![("proc".into(), "exec".into())]))
    }

    fn temp_workspace_root(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir()
            .join("zen-tests")
            .join(format!("{name}-{unique}"))
    }

    fn parse(src: &str) -> Program {
        let tokens = Lexer::new(src).tokenize().unwrap();
        let mut parser = Parser::new(tokens, src);
        parser.parse_program().unwrap()
    }

    fn plugin_inventory_entry(name: &str, kind: &str) -> Value {
        let mut map = HashMap::new();
        map.insert("name".into(), Value::String(name.into()));
        map.insert("kind".into(), Value::String(kind.into()));
        Value::Object(map)
    }

    fn exec_like_entry(stdout: &str, success: bool) -> Value {
        let mut map = HashMap::new();
        map.insert("stdout".into(), Value::String(stdout.into()));
        map.insert(
            "exitcode".into(),
            Value::Number(if success { 0.0 } else { 1.0 }),
        );
        map.insert("success".into(), Value::Bool(success));
        Value::Object(map)
    }

    #[test]
    fn split_external_command_line_preserves_quoted_arguments() {
        let args = Executor::split_external_command_line(
            r#".zen\plugins\sqlite\sqlite-plugin.exe query "C:\data\demo.db" "select id, body from notes""#,
        )
        .unwrap();

        assert_eq!(
            args,
            vec![
                r#".zen\plugins\sqlite\sqlite-plugin.exe"#,
                "query",
                r#"C:\data\demo.db"#,
                "select id, body from notes"
            ]
        );
    }

    #[test]
    fn split_external_command_line_rejects_unclosed_quotes() {
        let error = Executor::split_external_command_line(r#"sqlite.query "select 1"#)
            .expect_err("expected unclosed quote error");

        assert!(error.contains("unterminated"));
    }

    #[test]
    fn explicit_workspace_root_sets_root_and_initial_cwd() {
        let root = temp_workspace_root("explicit-workspace");
        std::fs::create_dir_all(&root).unwrap();

        let executor = Executor::new_with_permissions_and_workspace(
            PermissionSet::new(&Vec::new()),
            Some(root.clone()),
        )
        .unwrap();
        let canonical_root =
            Executor::normalize_canonical_path(std::fs::canonicalize(&root).unwrap());

        assert_eq!(executor.workspace_root_path(), canonical_root.as_path());
        assert_eq!(executor.cwd(), canonical_root.as_path());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn explicit_workspace_root_must_exist() {
        let root = temp_workspace_root("missing-workspace");

        let result = Executor::new_with_permissions_and_workspace(
            PermissionSet::new(&Vec::new()),
            Some(root),
        );
        let Err(err) = result else {
            panic!("expected invalid workspace error");
        };

        assert!(err.contains("Invalid workspace"));
    }

    fn plugin_inventory_has(value: &Value, name: &str) -> bool {
        let Value::List(entries) = value else {
            return false;
        };

        plugin_entry(entries, name).is_some()
    }

    fn plugin_entry<'a>(entries: &'a [Value], name: &str) -> Option<&'a HashMap<String, Value>> {
        entries.iter().find_map(|entry| {
            let Value::Object(map) = entry else {
                return None;
            };
            (map.get("name").and_then(Value::as_string) == Some(name)).then_some(map)
        })
    }

    fn executor_with_state_workspace(permissions: Vec<(String, String)>) -> Executor {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = env::current_dir()
            .unwrap()
            .join("target")
            .join("state-tests")
            .join(format!("{}-{}", std::process::id(), unique));
        fs::create_dir_all(&root).unwrap();

        let mut executor = Executor::new_with_permissions(PermissionSet::new(&permissions));
        executor.workspace_root = root.clone();
        executor.cwd = root;
        executor
    }

    #[test]
    fn new_with_plugins_uses_injected_plugin_dispatch() {
        let mut executor =
            Executor::new_with_plugins(PermissionSet::new(&Vec::new()), vec![Arc::new(TestPlugin)]);

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["test".into(), "ping".into()],
                args: Vec::new(),
                config: None,
            })
            .unwrap();

        assert_eq!(result.as_string(), Some("pong"));
    }

    #[test]
    fn if_statement_runs_then_branch() {
        let mut executor = executor();
        executor
            .execute(parse(
                "let ready = true\nif ready {\n  let state = \"dropbox-ready\"\n} else {\n  let state = \"dropbox-missing\"\n}\n",
            ))
            .unwrap();

        assert_eq!(
            executor.ctx.vars.get("state").and_then(Value::as_string),
            Some("dropbox-ready")
        );
    }

    #[test]
    fn if_statement_runs_else_branch() {
        let mut executor = executor();
        executor
            .execute(parse(
                "let ready = false\nif ready {\n  let state = \"dropbox-ready\"\n} else {\n  let state = \"dropbox-missing\"\n}\n",
            ))
            .unwrap();

        assert_eq!(
            executor.ctx.vars.get("state").and_then(Value::as_string),
            Some("dropbox-missing")
        );
    }

    #[test]
    fn if_condition_can_compare_object_field() {
        let mut executor = executor();
        executor
            .execute(parse(
                "let status = { auth: \"ok\" }\nif status.auth == \"ok\" {\n  let state = \"ready\"\n} else {\n  let state = \"missing\"\n}\n",
            ))
            .unwrap();

        assert_eq!(
            executor.ctx.vars.get("state").and_then(Value::as_string),
            Some("ready")
        );
    }

    #[test]
    fn if_condition_supports_logical_operators() {
        let mut executor = executor();
        executor
            .execute(parse(
                "let configured = true\nlet auth_ok = false\nif configured && !auth_ok {\n  let state = \"needs-auth\"\n} else {\n  let state = \"ready\"\n}\n",
            ))
            .unwrap();

        assert_eq!(
            executor.ctx.vars.get("state").and_then(Value::as_string),
            Some("needs-auth")
        );
    }

    #[test]
    fn if_condition_must_be_bool() {
        let mut executor = executor();
        let err = executor
            .execute(parse("if \"ok\" {\n  let state = \"ready\"\n}\n"))
            .unwrap_err();

        assert!(err.contains("if condition must evaluate to a boolean"));
    }

    #[test]
    fn try_catch_finally_handles_error_and_runs_cleanup() {
        let mut executor = executor();
        executor
            .execute(parse(
                "try {\n  unknown.command\n} catch error {\n  let handled = error\n} finally {\n  let cleaned = true\n}\n",
            ))
            .unwrap();

        match executor.ctx.vars.get("handled") {
            Some(Value::String(value)) => assert!(value.contains("Unknown function")),
            other => panic!("Expected handled error string, got {:?}", other),
        }
        assert!(matches!(
            executor.ctx.vars.get("cleaned"),
            Some(Value::Bool(true))
        ));
    }

    #[test]
    fn try_finally_propagates_error_after_cleanup() {
        let mut executor = executor();
        let err = executor
            .execute(parse(
                "try {\n  unknown.command\n} finally {\n  let cleaned = true\n}\n",
            ))
            .expect_err("expected try error");

        assert!(err.contains("Unknown function"));
        assert!(matches!(
            executor.ctx.vars.get("cleaned"),
            Some(Value::Bool(true))
        ));
    }

    #[test]
    fn workflow_run_executes_steps_and_finally_actions() {
        let _guard = crate::interrupt::lock_for_test();
        crate::interrupt::clear_interrupt();
        let mut executor = executor_with_exec_permission();
        executor
            .execute(parse(
                "let result = workflow.run { name: \"smoke\", steps: [{ name: \"one\", run: \"echo ok\", finally: [{ emit: \"cleanup.done\" }, { run: \"echo cleaned\" }] }] }\n",
            ))
            .unwrap();

        let Value::Object(result) = executor.ctx.vars.get("result").unwrap() else {
            panic!("Expected workflow result object");
        };
        assert!(matches!(result.get("success"), Some(Value::Bool(true))));
        let Value::List(steps) = result.get("steps").unwrap() else {
            panic!("Expected workflow steps");
        };
        let Value::Object(step) = &steps[0] else {
            panic!("Expected step object");
        };
        assert_eq!(
            step.get("status").and_then(Value::as_string),
            Some("succeeded")
        );
        let Value::List(events) = result.get("events").unwrap() else {
            panic!("Expected workflow events");
        };
        assert!(events.iter().any(|event| {
            let Value::Object(map) = event else {
                return false;
            };
            map.get("event").and_then(Value::as_string) == Some("cleanup.done")
        }));
    }

    #[test]
    fn workflow_run_retries_failure_and_runs_on_failure() {
        let _guard = crate::interrupt::lock_for_test();
        crate::interrupt::clear_interrupt();
        let mut executor = executor_with_exec_permission();
        executor
            .execute(parse(
                "let result = workflow.run { name: \"fail\", steps: [{ name: \"bad\", run: \"exit 7\", retry: { attempts: 2 }, on_failure: [{ emit: \"backup.dump.failed\" }], finally: [{ emit: \"cleanup.done\" }] }] }\n",
            ))
            .unwrap();

        let Value::Object(result) = executor.ctx.vars.get("result").unwrap() else {
            panic!("Expected workflow result object");
        };
        assert!(matches!(result.get("success"), Some(Value::Bool(false))));
        let Value::List(steps) = result.get("steps").unwrap() else {
            panic!("Expected workflow steps");
        };
        let Value::Object(step) = &steps[0] else {
            panic!("Expected step object");
        };
        assert_eq!(
            step.get("status").and_then(Value::as_string),
            Some("failed")
        );
        assert!(matches!(step.get("attempts"), Some(Value::Number(2.0))));
        let Value::List(events) = result.get("events").unwrap() else {
            panic!("Expected workflow events");
        };
        assert!(events.iter().any(|event| {
            let Value::Object(map) = event else {
                return false;
            };
            map.get("event").and_then(Value::as_string) == Some("step.retrying")
        }));
        assert!(events.iter().any(|event| {
            let Value::Object(map) = event else {
                return false;
            };
            map.get("event").and_then(Value::as_string) == Some("backup.dump.failed")
        }));
    }

    #[test]
    fn workflow_run_collects_step_outputs() {
        let _guard = crate::interrupt::lock_for_test();
        crate::interrupt::clear_interrupt();
        let mut executor = executor_with_exec_permission();
        executor
            .execute(parse(
                "let result = workflow.run { name: \"outputs-smoke\", steps: [{ name: \"dump-db\", run: \"echo backup\", save_as: \"dump\" }] }\n",
            ))
            .unwrap();

        let Value::Object(result) = executor.ctx.vars.get("result").unwrap() else {
            panic!("Expected workflow result object");
        };
        let Value::Object(outputs) = result.get("outputs").unwrap() else {
            panic!("Expected workflow outputs");
        };
        let Value::Object(dump) = outputs.get("dump").unwrap() else {
            panic!("Expected dump output");
        };
        assert_eq!(
            dump.get("stdout").and_then(Value::as_string).map(str::trim),
            Some("backup")
        );
    }

    #[test]
    fn workflow_run_collects_artifact_summaries() {
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join("workflow-artifacts-test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("source.txt"), "artifact").unwrap();

        let mut executor = Executor::new_with_permissions(PermissionSet::new(&vec![
            ("fs".into(), "read".into()),
            ("fs".into(), "write".into()),
        ]));
        executor.cwd = root.clone();
        executor.workspace_root = root.clone();
        executor
            .execute(parse(
                "let result = workflow.run { name: \"artifact-smoke\", steps: [{ name: \"copy\", zen: \"fs.copy \\\"source.txt\\\" \\\"dist/artifact.txt\\\"\", artifacts: [{ name: \"copied\", path: \"dist/artifact.txt\" }] }] }\n",
            ))
            .unwrap();

        let Value::Object(result) = executor.ctx.vars.get("result").unwrap() else {
            panic!("Expected workflow result object");
        };
        let Value::List(artifacts) = result.get("artifacts").unwrap() else {
            panic!("Expected workflow artifacts");
        };
        let Value::Object(artifact) = &artifacts[0] else {
            panic!("Expected artifact summary object");
        };
        assert_eq!(
            artifact.get("name").and_then(Value::as_string),
            Some("copied")
        );
        assert_eq!(
            artifact.get("step").and_then(Value::as_string),
            Some("copy")
        );
        assert_eq!(
            artifact.get("path").and_then(Value::as_string),
            Some("dist/artifact.txt")
        );
        assert!(matches!(artifact.get("exists"), Some(Value::Bool(true))));
        assert!(matches!(artifact.get("size"), Some(Value::Number(8.0))));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workflow_run_skips_step_when_if_condition_is_false() {
        let _guard = crate::interrupt::lock_for_test();
        crate::interrupt::clear_interrupt();
        let mut executor = executor_with_exec_permission();
        executor
            .execute(parse(
                "let result = workflow.run { name: \"if-smoke\", steps: [{ name: \"probe\", run: \"echo ok\", save_as: \"probe\" }, { name: \"skip\", if: \"outputs.probe.success == false\", run: \"echo should-not-run\" }, { name: \"run\", if: \"outputs.probe.success != false\", run: \"echo should-run\" }] }\n",
            ))
            .unwrap();

        let Value::Object(result) = executor.ctx.vars.get("result").unwrap() else {
            panic!("Expected workflow result object");
        };
        let Value::List(steps) = result.get("steps").unwrap() else {
            panic!("Expected workflow steps");
        };
        let Value::Object(skipped) = &steps[1] else {
            panic!("Expected skipped step object");
        };
        assert_eq!(
            skipped.get("status").and_then(Value::as_string),
            Some("skipped")
        );
        let Value::Object(ran) = &steps[2] else {
            panic!("Expected ran step object");
        };
        assert_eq!(
            ran.get("status").and_then(Value::as_string),
            Some("succeeded")
        );
        let Value::List(events) = result.get("events").unwrap() else {
            panic!("Expected workflow events");
        };
        assert!(events.iter().any(|event| {
            let Value::Object(map) = event else {
                return false;
            };
            map.get("event").and_then(Value::as_string) == Some("step.skipped")
                && map.get("step").and_then(Value::as_string) == Some("skip")
        }));
    }

    #[test]
    fn workflow_run_executes_zen_step_in_process() {
        let mut executor = executor_with_exec_permission();
        executor
            .execute(parse(
                "let result = workflow.run { name: \"zen-smoke\", steps: [{ name: \"add\", zen: \"math.add 2 3\", save_as: \"sum\" }, { name: \"after\", if: \"outputs.sum == 5\", zen: \"echo ok\" }] }\n",
            ))
            .unwrap();

        let Value::Object(result) = executor.ctx.vars.get("result").unwrap() else {
            panic!("Expected workflow result object");
        };
        assert!(matches!(result.get("success"), Some(Value::Bool(true))));
        let Value::Object(outputs) = result.get("outputs").unwrap() else {
            panic!("Expected outputs");
        };
        assert!(matches!(outputs.get("sum"), Some(Value::Number(5.0))));
        let Value::List(steps) = result.get("steps").unwrap() else {
            panic!("Expected steps");
        };
        let Value::Object(after) = &steps[1] else {
            panic!("Expected after step object");
        };
        assert_eq!(
            after.get("status").and_then(Value::as_string),
            Some("succeeded")
        );
    }

    #[test]
    fn fs_copy_copies_single_file_inside_workspace() {
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join("fs-copy-test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("source")).unwrap();
        fs::write(root.join("source").join("input.txt"), "copy me").unwrap();

        let mut executor = Executor::new_with_permissions(PermissionSet::new(&vec![
            ("fs".into(), "read".into()),
            ("fs".into(), "write".into()),
        ]));
        executor.cwd = root.clone();
        executor.workspace_root = root.clone();

        let result = executor
            .fs_copy_builtin(
                vec![
                    Expr::String("source/input.txt".into()),
                    Expr::String("dist/output.txt".into()),
                ],
                Value::Null,
            )
            .unwrap();

        let Value::Object(map) = result else {
            panic!("Expected copy result object");
        };
        assert!(matches!(map.get("success"), Some(Value::Bool(true))));
        assert_eq!(
            fs::read_to_string(root.join("dist").join("output.txt")).unwrap(),
            "copy me"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workflow_run_passes_step_env_to_command() {
        let _guard = crate::interrupt::lock_for_test();
        crate::interrupt::clear_interrupt();
        let mut executor = executor_with_exec_permission();
        let command = if cfg!(windows) {
            "powershell -NoProfile -Command $env:ZEN_WORKFLOW_TEST"
        } else {
            "printenv ZEN_WORKFLOW_TEST"
        };
        executor
            .execute(parse(&format!(
                "let result = workflow.run {{ name: \"env-smoke\", steps: [{{ name: \"env\", run: \"{}\", env: {{ ZEN_WORKFLOW_TEST: \"hello\" }} }}] }}\n",
                command
            )))
            .unwrap();

        let Value::Object(result) = executor.ctx.vars.get("result").unwrap() else {
            panic!("Expected workflow result object");
        };
        let Value::List(steps) = result.get("steps").unwrap() else {
            panic!("Expected workflow steps");
        };
        let Value::Object(step) = &steps[0] else {
            panic!("Expected step object");
        };
        let Value::Object(output) = step.get("output").unwrap() else {
            panic!("Expected step output");
        };
        assert_eq!(
            output
                .get("stdout")
                .and_then(Value::as_string)
                .map(str::trim),
            Some("hello")
        );
    }

    #[test]
    fn workflow_run_resolves_secret_env_from_store() {
        let _guard = crate::interrupt::lock_for_test();
        crate::interrupt::clear_interrupt();
        let secret_name = format!("zen.test.canary.{}", std::process::id());
        let canary = "ZEN_CANARY_SECRET_plaintext_must_not_persist";
        crate::runtime::plugins::secrets::write_secret(&secret_name, canary).unwrap();

        let mut executor = Executor::new_with_permissions(PermissionSet::new(&vec![
            ("proc".into(), "exec".into()),
            ("secrets".into(), "read".into()),
        ]));
        // Compares the injected env var against the expected value via exit
        // code rather than echoing it to stdout, so this test proves
        // delivery without ever printing the plaintext secret anywhere.
        let command = workflow_secret_comparison_command(canary);
        let result = executor.execute(parse(&format!(
            "let result = workflow.run {{ name: \"secret-smoke\", steps: [{{ name: \"env\", run: \"{}\", env: {{ ZEN_WORKFLOW_SECRET: {{ secret: \"{}\" }} }} }}] }}\n",
            command, secret_name
        )));

        crate::runtime::plugins::secrets::delete_secret(&secret_name).unwrap();
        result.unwrap();

        let Value::Object(result) = executor.ctx.vars.get("result").unwrap() else {
            panic!("Expected workflow result object");
        };
        let Value::List(steps) = result.get("steps").unwrap() else {
            panic!("Expected workflow steps");
        };
        let Value::Object(step) = &steps[0] else {
            panic!("Expected step object");
        };
        let Value::Object(output) = step.get("output").unwrap() else {
            panic!("Expected step output");
        };
        assert!(
            matches!(output.get("success"), Some(Value::Bool(true))),
            "child process should have observed the resolved secret value: {:?}",
            output
        );
    }

    #[test]
    fn workflow_run_secret_env_requires_secrets_read_permission() {
        let _guard = crate::interrupt::lock_for_test();
        crate::interrupt::clear_interrupt();
        let mut executor = executor_with_exec_permission();
        let err = executor
            .execute(parse(
                "workflow.run { name: \"secret-perm\", steps: [{ name: \"env\", run: \"echo hi\", env: { X: { secret: \"zen.test.perm.missing\" } } }] }\n",
            ))
            .unwrap_err();
        assert!(err.contains("secrets.read"));
    }

    #[test]
    fn workflow_run_secret_env_reports_missing_secret() {
        let _guard = crate::interrupt::lock_for_test();
        crate::interrupt::clear_interrupt();
        let missing_name = format!("zen.test.missing.{}", std::process::id());
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&vec![
            ("proc".into(), "exec".into()),
            ("secrets".into(), "read".into()),
        ]));
        let err = executor
            .execute(parse(&format!(
                "workflow.run {{ name: \"missing-secret\", steps: [{{ name: \"env\", run: \"echo hi\", env: {{ X: {{ secret: \"{}\" }} }} }}] }}\n",
                missing_name
            )))
            .unwrap_err();
        assert!(err.contains("was not found"));
    }

    #[test]
    fn workflow_validation_rejects_malformed_secret_env_entries() {
        let mut executor = executor_with_exec_permission();
        let err = executor
            .execute(parse(
                "workflow.run { name: \"bad-secret-env\", steps: [{ name: \"s\", run: \"echo hi\", env: { A: { secret: 7 }, B: { secret: \"\" }, C: { secret: \"ok\", extra: \"x\" }, D: { notsecret: \"x\" } } }] }\n",
            ))
            .expect_err("expected workflow validation error");

        assert!(err.contains("steps[0].env.A must be a string or"));
        assert!(err.contains("steps[0].env.B must be a string or"));
        assert!(err.contains("steps[0].env.C must be a string or"));
        assert!(err.contains("steps[0].env.D must be a string or"));
    }

    #[test]
    fn workflow_run_persisted_never_stores_secret_plaintext() {
        let _guard = crate::interrupt::lock_for_test();
        crate::interrupt::clear_interrupt();
        let secret_name = format!("zen.test.persist.{}", std::process::id());
        let canary = "ZEN_CANARY_SECRET_plaintext_must_not_persist";
        crate::runtime::plugins::secrets::write_secret(&secret_name, canary).unwrap();

        let mut executor = executor_with_state_workspace(vec![
            ("proc".into(), "exec".into()),
            ("secrets".into(), "read".into()),
        ]);
        let command = workflow_secret_comparison_command(canary);
        let workflow = parse(&format!(
            "workflow.run {{ name: \"persist-secret-smoke\", steps: [{{ name: \"env\", run: \"{}\", env: {{ ZEN_WORKFLOW_SECRET: {{ secret: \"{}\" }} }} }}] }}\n",
            command, secret_name
        ));
        let Stmt::Expr(Expr::Call(call)) = workflow.statements.into_iter().next().unwrap() else {
            panic!("Expected workflow call");
        };
        let value = executor.eval_echo_arg(call.args[0].clone()).unwrap();
        let result = executor.workflow_run_persisted(value, "workflow.yaml");

        let db_path = executor.workflow_runtime_db_path();
        let db_bytes = fs::read(&db_path).unwrap();
        let db_text = String::from_utf8_lossy(&db_bytes);

        crate::runtime::plugins::secrets::delete_secret(&secret_name).unwrap();

        let Value::Object(result) = result.unwrap() else {
            panic!("Expected workflow result");
        };
        assert!(matches!(result.get("success"), Some(Value::Bool(true))));
        assert!(
            !db_text.contains(canary),
            "runtime.db must never contain the plaintext secret"
        );

        // Positive control: confirm the step actually ran and received the
        // resolved secret (via exit code, never printed), so the absence
        // check above isn't vacuous.
        let Value::List(steps) = result.get("steps").unwrap() else {
            panic!("Expected workflow steps");
        };
        let Value::Object(step) = &steps[0] else {
            panic!("Expected step object");
        };
        let Value::Object(output) = step.get("output").unwrap() else {
            panic!("Expected step output");
        };
        assert!(
            matches!(output.get("success"), Some(Value::Bool(true))),
            "child process should have observed the resolved secret value: {:?}",
            output
        );
    }

    /// Builds a shell command that succeeds only if `ZEN_WORKFLOW_SECRET`
    /// equals `expected`, without ever printing the value — used so
    /// secret-delivery tests can prove the correct value reached the child
    /// process while still asserting the value appears nowhere else.
    fn workflow_secret_comparison_command(expected: &str) -> String {
        if cfg!(windows) {
            format!(
                "if %ZEN_WORKFLOW_SECRET%=={} (exit 0) else (exit 1)",
                expected
            )
        } else {
            format!("test $ZEN_WORKFLOW_SECRET = {}", expected)
        }
    }

    #[test]
    fn workflow_run_timeout_marks_step_failed() {
        let _guard = crate::interrupt::lock_for_test();
        crate::interrupt::clear_interrupt();
        let mut executor = executor_with_exec_permission();
        let command = if cfg!(windows) {
            "powershell -NoProfile -Command Start-Sleep -Seconds 2"
        } else {
            "sleep 2"
        };
        executor
            .execute(parse(&format!(
                "let result = workflow.run {{ name: \"timeout-smoke\", steps: [{{ name: \"slow\", run: \"{}\", timeout: \"10ms\" }}] }}\n",
                command
            )))
            .unwrap();

        let Value::Object(result) = executor.ctx.vars.get("result").unwrap() else {
            panic!("Expected workflow result object");
        };
        assert!(matches!(result.get("success"), Some(Value::Bool(false))));
        let Value::List(steps) = result.get("steps").unwrap() else {
            panic!("Expected workflow steps");
        };
        let Value::Object(step) = &steps[0] else {
            panic!("Expected step object");
        };
        let Value::Object(output) = step.get("output").unwrap() else {
            panic!("Expected step output");
        };
        assert!(matches!(output.get("timed_out"), Some(Value::Bool(true))));
    }

    #[test]
    fn workflow_run_runs_rollback_after_failure_actions_before_finally() {
        let _guard = crate::interrupt::lock_for_test();
        crate::interrupt::clear_interrupt();
        let mut executor = executor_with_exec_permission();
        executor
            .execute(parse(
                "let result = workflow.run { name: \"rollback-smoke\", steps: [{ name: \"bad\", run: \"exit 7\", on_failure: [{ emit: \"step.failure\" }], rollback: [{ emit: \"step.rollback\" }, { run: \"echo rollback\" }], finally: [{ emit: \"step.finally\" }] }] }\n",
            ))
            .unwrap();

        let Value::Object(result) = executor.ctx.vars.get("result").unwrap() else {
            panic!("Expected workflow result object");
        };
        assert!(matches!(result.get("success"), Some(Value::Bool(false))));
        let Value::List(events) = result.get("events").unwrap() else {
            panic!("Expected workflow events");
        };
        let names: Vec<_> = events
            .iter()
            .filter_map(|event| {
                let Value::Object(map) = event else {
                    return None;
                };
                map.get("event").and_then(Value::as_string)
            })
            .collect();
        let failure_index = names
            .iter()
            .position(|event| *event == "step.failure")
            .expect("Expected failure event");
        let rollback_index = names
            .iter()
            .position(|event| *event == "step.rollback")
            .expect("Expected rollback event");
        let rollback_run_index = names
            .iter()
            .position(|event| *event == "rollback.succeeded")
            .expect("Expected rollback run event");
        let finally_index = names
            .iter()
            .position(|event| *event == "step.finally")
            .expect("Expected finally event");

        assert!(failure_index < rollback_index);
        assert!(rollback_index < rollback_run_index);
        assert!(rollback_run_index < finally_index);
    }

    #[test]
    fn workflow_run_rolls_back_completed_steps_in_reverse_order() {
        let _guard = crate::interrupt::lock_for_test();
        crate::interrupt::clear_interrupt();
        let mut executor = executor_with_exec_permission();
        executor
            .execute(parse(
                "let result = workflow.run { name: \"unwind-smoke\", steps: [{ name: \"one\", run: \"echo one\", rollback: [{ emit: \"rollback.one\" }] }, { name: \"two\", run: \"echo two\", rollback: [{ emit: \"rollback.two\" }] }, { name: \"three\", run: \"exit 7\" }] }\n",
            ))
            .unwrap();

        let Value::Object(result) = executor.ctx.vars.get("result").unwrap() else {
            panic!("Expected workflow result object");
        };
        assert!(matches!(result.get("success"), Some(Value::Bool(false))));
        let Value::List(steps) = result.get("steps").unwrap() else {
            panic!("Expected workflow steps");
        };
        for step in steps.iter().take(2) {
            let Value::Object(step) = step else {
                panic!("Expected step object");
            };
            assert_eq!(
                step.get("status").and_then(Value::as_string),
                Some("rolled_back")
            );
        }

        let Value::List(events) = result.get("events").unwrap() else {
            panic!("Expected workflow events");
        };
        let names: Vec<_> = events
            .iter()
            .filter_map(|event| {
                let Value::Object(map) = event else {
                    return None;
                };
                map.get("event").and_then(Value::as_string)
            })
            .collect();
        let rollback_two = names
            .iter()
            .position(|event| *event == "rollback.two")
            .expect("Expected rollback.two");
        let rollback_one = names
            .iter()
            .position(|event| *event == "rollback.one")
            .expect("Expected rollback.one");

        assert!(rollback_two < rollback_one);
    }

    #[test]
    fn workflow_resume_persisted_resumes_from_checkpoint() {
        let _guard = crate::interrupt::lock_for_test();
        crate::interrupt::clear_interrupt();
        let mut executor = executor_with_state_workspace(vec![("proc".into(), "exec".into())]);

        let first = parse(
            "workflow.run { name: \"resume-smoke\", steps: [{ name: \"prepare\", run: \"echo prepared\", checkpoint: \"prepared\", save_as: \"prep\" }, { name: \"fail\", run: \"exit 9\" }] }\n",
        );
        let Stmt::Expr(Expr::Call(call)) = first.statements.into_iter().next().unwrap() else {
            panic!("Expected workflow call");
        };
        let first_value = executor.eval_echo_arg(call.args[0].clone()).unwrap();
        let first_result = executor
            .workflow_run_persisted(first_value, "workflow.yaml")
            .unwrap();
        let Value::Object(first_map) = first_result else {
            panic!("Expected workflow result");
        };
        assert!(matches!(first_map.get("success"), Some(Value::Bool(false))));
        let run_id = first_map
            .get("run_id")
            .and_then(Value::as_string)
            .expect("Expected run id")
            .to_string();

        let second = parse(
            "workflow.run { name: \"resume-smoke\", steps: [{ name: \"prepare\", run: \"exit 9\", checkpoint: \"prepared\", save_as: \"prep\" }, { name: \"finish\", run: \"echo done\" }] }\n",
        );
        let Stmt::Expr(Expr::Call(call)) = second.statements.into_iter().next().unwrap() else {
            panic!("Expected workflow call");
        };
        let second_value = executor.eval_echo_arg(call.args[0].clone()).unwrap();
        let second_result = executor
            .workflow_resume_persisted(second_value, "workflow.yaml", &run_id)
            .unwrap();
        let Value::Object(second_map) = second_result else {
            panic!("Expected workflow result");
        };
        assert!(matches!(second_map.get("success"), Some(Value::Bool(true))));
        let Value::List(steps) = second_map.get("steps").unwrap() else {
            panic!("Expected steps");
        };
        let Value::Object(first_step) = &steps[0] else {
            panic!("Expected first step");
        };
        assert_eq!(
            first_step.get("status").and_then(Value::as_string),
            Some("skipped")
        );
        let Value::Object(outputs) = second_map.get("outputs").unwrap() else {
            panic!("Expected outputs");
        };
        let Value::Object(prep) = outputs.get("prep").unwrap() else {
            panic!("Expected resumed output");
        };
        assert_eq!(
            prep.get("stdout").and_then(Value::as_string).map(str::trim),
            Some("prepared")
        );
    }

    #[test]
    fn workflow_run_persisted_starts_new_run_without_implicit_resume() {
        let _guard = crate::interrupt::lock_for_test();
        crate::interrupt::clear_interrupt();
        let mut executor = executor_with_state_workspace(vec![("proc".into(), "exec".into())]);

        let first = parse(
            "workflow.run { name: \"fresh-smoke\", steps: [{ name: \"prepare\", run: \"echo prepared\", checkpoint: \"prepared\" }, { name: \"fail\", run: \"exit 9\" }] }\n",
        );
        let Stmt::Expr(Expr::Call(call)) = first.statements.into_iter().next().unwrap() else {
            panic!("Expected workflow call");
        };
        let first_value = executor.eval_echo_arg(call.args[0].clone()).unwrap();
        let first_result = executor
            .workflow_run_persisted(first_value, "workflow.yaml")
            .unwrap();
        let Value::Object(first_map) = first_result else {
            panic!("Expected workflow result");
        };
        let first_run_id = first_map
            .get("run_id")
            .and_then(Value::as_string)
            .expect("Expected run id")
            .to_string();

        let second = parse(
            "workflow.run { name: \"fresh-smoke\", steps: [{ name: \"prepare\", run: \"exit 9\", checkpoint: \"prepared\" }] }\n",
        );
        let Stmt::Expr(Expr::Call(call)) = second.statements.into_iter().next().unwrap() else {
            panic!("Expected workflow call");
        };
        let second_value = executor.eval_echo_arg(call.args[0].clone()).unwrap();
        let second_result = executor
            .workflow_run_persisted(second_value, "workflow.yaml")
            .unwrap();
        let Value::Object(second_map) = second_result else {
            panic!("Expected workflow result");
        };
        let second_run_id = second_map
            .get("run_id")
            .and_then(Value::as_string)
            .expect("Expected run id");
        assert_ne!(first_run_id, second_run_id);
        assert!(matches!(
            second_map.get("success"),
            Some(Value::Bool(false))
        ));
        let Value::List(steps) = second_map.get("steps").unwrap() else {
            panic!("Expected steps");
        };
        let Value::Object(first_step) = &steps[0] else {
            panic!("Expected first step");
        };
        assert_eq!(
            first_step.get("status").and_then(Value::as_string),
            Some("failed")
        );
    }

    #[test]
    fn workflow_validation_reports_multiple_path_errors() {
        let mut executor = executor_with_exec_permission();
        let err = executor
            .execute(parse(
                "workflow.run { steps: [{ if: false, timeout: false, env: { OK: \"yes\", BAD: 7 }, save_as: false, retry: { attempts: 0, delay: false }, rollback: { run: \"cleanup\" }, on_failure: [{ run: \"a\", emit: \"b\" }, {}] }, false] }\n",
            ))
            .expect_err("expected workflow validation error");

        assert!(err.contains("Workflow validation failed"));
        assert!(err.contains("name is required"));
        assert!(err.contains("steps[0].name is required"));
        assert!(err.contains("steps[0] must contain either run or zen"));
        assert!(err.contains("steps[0].if must be a string"));
        assert!(err.contains("steps[0].timeout must be a duration string or number"));
        assert!(err.contains("steps[0].env.BAD must be a string"));
        assert!(err.contains("steps[0].save_as must be a string"));
        assert!(err.contains("steps[0].retry.attempts must be an integer >= 1"));
        assert!(err.contains("steps[0].retry.delay must be a duration string or number"));
        assert!(err.contains("steps[0].rollback must be a list of actions"));
        assert!(err.contains("steps[0].on_failure[0] must contain only one of run or emit"));
        assert!(err.contains("steps[0].on_failure[1] must contain either run or emit"));
        assert!(err.contains("steps[1] must be an object"));
    }

    #[test]
    fn new_with_plugins_uses_injected_plugin_command_discovery() {
        let mut executor = Executor::new_with_plugins(
            PermissionSet::new(&Vec::new()),
            vec![
                Arc::new(crate::runtime::plugins::core::CorePlugin),
                Arc::new(TestPlugin),
            ],
        );

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["which".into()],
                args: vec![Expr::Call(FunctionCall {
                    name: vec!["test".into(), "ping".into()],
                    args: Vec::new(),
                    config: None,
                })],
                config: None,
            })
            .unwrap();

        assert_eq!(result.as_string(), Some("builtin:test.ping"));
    }

    #[test]
    fn math_plugin_adds_numbers() {
        let mut executor = executor();

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["math".into(), "add".into()],
                args: vec![Expr::Number(1.0), Expr::Number(2.0), Expr::Number(3.0)],
                config: None,
            })
            .unwrap();

        assert_eq!(result.as_number(), Some(6.0));
    }

    #[test]
    fn math_plugin_divides_numbers() {
        let mut executor = executor();

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["math".into(), "div".into()],
                args: vec![Expr::Number(100.0), Expr::Number(2.0), Expr::Number(5.0)],
                config: None,
            })
            .unwrap();

        assert_eq!(result.as_number(), Some(10.0));
    }

    #[test]
    fn math_plugin_rejects_divide_by_zero() {
        let mut executor = executor();

        let err = executor
            .eval_call(FunctionCall {
                name: vec!["math".into(), "div".into()],
                args: vec![Expr::Number(10.0), Expr::Number(0.0)],
                config: None,
            })
            .unwrap_err();

        assert_eq!(err, "math.div cannot divide by zero");
    }

    #[test]
    fn math_plugin_uses_variables_as_args() {
        let mut executor = executor();
        executor.ctx.vars.insert("left".into(), Value::Number(8.0));

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["math".into(), "mul".into()],
                args: vec![Expr::Ident("left".into()), Expr::String("3".into())],
                config: None,
            })
            .unwrap();

        assert_eq!(result.as_number(), Some(24.0));
    }

    #[test]
    fn dollar_variables_must_exist() {
        let mut executor = executor();

        let err = executor
            .eval_echo_arg(Expr::Variable("missing".into()))
            .unwrap_err();

        assert_eq!(err, "Undefined variable '$missing'");
    }

    #[test]
    fn bare_ident_command_args_still_fall_back_to_text() {
        let mut executor = executor();

        let result = executor
            .eval_echo_arg(Expr::Ident("missing".into()))
            .unwrap();

        assert_eq!(result.as_string(), Some("missing"));
    }

    #[test]
    fn string_plugin_transforms_direct_text() {
        let mut executor = executor();

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["string".into(), "upper".into()],
                args: vec![Expr::String("hello".into())],
                config: None,
            })
            .unwrap();

        assert_eq!(result.as_string(), Some("HELLO"));
    }

    #[test]
    fn string_plugin_transforms_pipeline_text() {
        let mut executor = executor();

        let result = executor
            .eval_call_with_input(
                FunctionCall {
                    name: vec!["string".into(), "trim".into()],
                    args: Vec::new(),
                    config: None,
                },
                Value::String("  hello  ".into()),
            )
            .unwrap();

        assert_eq!(result.as_string(), Some("hello"));
    }

    #[test]
    fn string_plugin_reports_length_by_characters() {
        let mut executor = executor();

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["string".into(), "len".into()],
                args: vec![Expr::String("zest".into())],
                config: None,
            })
            .unwrap();

        assert_eq!(result.as_number(), Some(4.0));
    }

    #[test]
    fn string_plugin_checks_contains_direct_and_pipeline_text() {
        let mut executor = executor();

        let direct = executor
            .eval_call(FunctionCall {
                name: vec!["string".into(), "contains".into()],
                args: vec![
                    Expr::String("hello world".into()),
                    Expr::String("world".into()),
                ],
                config: None,
            })
            .unwrap();

        let piped = executor
            .eval_call_with_input(
                FunctionCall {
                    name: vec!["string".into(), "contains".into()],
                    args: vec![Expr::String("zen".into())],
                    config: None,
                },
                Value::String("hello zen".into()),
            )
            .unwrap();

        assert!(matches!(direct, Value::Bool(true)));
        assert!(matches!(piped, Value::Bool(true)));
    }

    #[test]
    fn string_plugin_replaces_direct_and_pipeline_text() {
        let mut executor = executor();

        let direct = executor
            .eval_call(FunctionCall {
                name: vec!["string".into(), "replace".into()],
                args: vec![
                    Expr::String("hello world".into()),
                    Expr::String("world".into()),
                    Expr::String("zen".into()),
                ],
                config: None,
            })
            .unwrap();

        let piped = executor
            .eval_call_with_input(
                FunctionCall {
                    name: vec!["string".into(), "replace".into()],
                    args: vec![Expr::String("world".into()), Expr::String("zen".into())],
                    config: None,
                },
                Value::String("hello world".into()),
            )
            .unwrap();

        assert_eq!(direct.as_string(), Some("hello zen"));
        assert_eq!(piped.as_string(), Some("hello zen"));
    }

    #[test]
    fn string_plugin_splits_direct_and_pipeline_text() {
        let mut executor = executor();

        let direct = executor
            .eval_call(FunctionCall {
                name: vec!["string".into(), "split".into()],
                args: vec![Expr::String("a,b,c".into()), Expr::String(",".into())],
                config: None,
            })
            .unwrap();

        let piped = executor
            .eval_call_with_input(
                FunctionCall {
                    name: vec!["string".into(), "split".into()],
                    args: vec![Expr::String(",".into())],
                    config: None,
                },
                Value::String("a,b,c".into()),
            )
            .unwrap();

        for result in [direct, piped] {
            let Value::List(items) = result else {
                panic!("Expected split result list");
            };

            let parts: Vec<_> = items.iter().map(|item| item.as_string().unwrap()).collect();
            assert_eq!(parts, vec!["a", "b", "c"]);
        }
    }

    #[test]
    fn string_plugin_joins_variable_and_pipeline_lists() {
        let mut executor = executor();
        executor.ctx.vars.insert(
            "parts".into(),
            Value::List(vec![
                Value::String("a".into()),
                Value::String("b".into()),
                Value::String("c".into()),
            ]),
        );

        let direct = executor
            .eval_call(FunctionCall {
                name: vec!["string".into(), "join".into()],
                args: vec![Expr::Ident("parts".into()), Expr::String(" | ".into())],
                config: None,
            })
            .unwrap();

        let piped = executor
            .eval_call_with_input(
                FunctionCall {
                    name: vec!["string".into(), "join".into()],
                    args: vec![Expr::String(" | ".into())],
                    config: None,
                },
                Value::List(vec![
                    Value::String("a".into()),
                    Value::String("b".into()),
                    Value::String("c".into()),
                ]),
            )
            .unwrap();

        assert_eq!(direct.as_string(), Some("a | b | c"));
        assert_eq!(piped.as_string(), Some("a | b | c"));
    }

    #[test]
    fn list_literals_evaluate_to_lists() {
        let mut ctx = Context::new();

        let result = Executor::eval_expr(
            &Expr::List(vec![
                Expr::String("a".into()),
                Expr::Number(2.0),
                Expr::Bool(true),
            ]),
            &mut ctx,
        )
        .unwrap();

        let Value::List(items) = result else {
            panic!("Expected list value");
        };

        assert_eq!(items[0].as_string(), Some("a"));
        assert_eq!(items[1].as_number(), Some(2.0));
        assert!(matches!(items[2], Value::Bool(true)));
    }

    #[test]
    fn object_literals_evaluate_to_objects() {
        let mut ctx = Context::new();

        let result = Executor::eval_expr(
            &Expr::Object(vec![
                ("name".into(), Expr::String("zen".into())),
                ("count".into(), Expr::Number(3.0)),
                (
                    "tags".into(),
                    Expr::List(vec![
                        Expr::String("cli".into()),
                        Expr::String("rust".into()),
                    ]),
                ),
                (
                    "meta".into(),
                    Expr::Object(vec![("active".into(), Expr::Bool(true))]),
                ),
                ("missing".into(), Expr::Literal(Literal::Null)),
            ]),
            &mut ctx,
        )
        .unwrap();

        let Value::Object(map) = result else {
            panic!("Expected object value");
        };

        assert_eq!(map.get("name").and_then(Value::as_string), Some("zen"));
        assert_eq!(map.get("count").and_then(Value::as_number), Some(3.0));
        assert!(matches!(map.get("tags"), Some(Value::List(items)) if items.len() == 2));
        assert!(matches!(
            map.get("meta"),
            Some(Value::Object(meta)) if matches!(meta.get("active"), Some(Value::Bool(true)))
        ));
        assert!(matches!(map.get("missing"), Some(Value::Null)));
    }

    #[test]
    fn string_plugin_joins_list_literal() {
        let mut executor = executor();

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["string".into(), "join".into()],
                args: vec![
                    Expr::List(vec![
                        Expr::String("a".into()),
                        Expr::String("b".into()),
                        Expr::String("c".into()),
                    ]),
                    Expr::String(",".into()),
                ],
                config: None,
            })
            .unwrap();

        assert_eq!(result.as_string(), Some("a,b,c"));
    }

    #[test]
    fn string_plugin_checks_prefix_and_suffix() {
        let mut executor = executor();

        let starts = executor
            .eval_call(FunctionCall {
                name: vec!["string".into(), "starts_with".into()],
                args: vec![Expr::String("hello".into()), Expr::String("he".into())],
                config: None,
            })
            .unwrap();

        let ends = executor
            .eval_call_with_input(
                FunctionCall {
                    name: vec!["string".into(), "ends_with".into()],
                    args: vec![Expr::String("lo".into())],
                    config: None,
                },
                Value::String("hello".into()),
            )
            .unwrap();

        assert!(matches!(starts, Value::Bool(true)));
        assert!(matches!(ends, Value::Bool(true)));
    }

    #[test]
    fn workspace_root_and_cwd_report_paths_without_permission() {
        let mut executor = executor();

        let root = executor
            .eval_call(FunctionCall {
                name: vec!["workspace".into(), "root".into()],
                args: Vec::new(),
                config: None,
            })
            .unwrap();
        let cwd = executor
            .eval_call(FunctionCall {
                name: vec!["workspace".into(), "cwd".into()],
                args: Vec::new(),
                config: None,
            })
            .unwrap();

        let root = PathBuf::from(root.as_string().expect("Expected workspace root"));
        let cwd = PathBuf::from(cwd.as_string().expect("Expected workspace cwd"));

        assert!(root.join("Cargo.toml").is_file());
        assert!(cwd.is_dir());
    }

    #[test]
    fn local_write_paths_reject_parent_traversal() {
        let executor = executor_with_state_workspace(Vec::new());

        let err = executor
            .resolve_local_write_path("../outside.txt")
            .unwrap_err();

        assert_eq!(err, "local write paths cannot traverse parent directories");
    }

    #[test]
    fn local_write_paths_resolve_relative_to_session_cwd() {
        let executor = executor_with_state_workspace(Vec::new());

        let path = executor
            .resolve_local_write_path("downloads/report.txt")
            .unwrap();

        assert!(path.ends_with("downloads/report.txt"));
        assert!(path.starts_with(&executor.cwd));
    }

    #[test]
    fn workspace_find_requires_permission() {
        let mut executor = executor();

        let err = executor
            .eval_call(FunctionCall {
                name: vec!["workspace".into(), "find".into()],
                args: vec![Expr::String("*.rs".into())],
                config: None,
            })
            .unwrap_err();

        assert!(err.contains("workspace.read"));
    }

    #[test]
    fn workspace_find_returns_matching_files() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&vec![(
            "workspace".into(),
            "read".into(),
        )]));

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["workspace".into(), "find".into()],
                args: vec![Expr::String("*.rs".into())],
                config: None,
            })
            .unwrap();

        let Value::List(files) = result else {
            panic!("Expected file list");
        };

        assert!(files.iter().any(|file| {
            let Value::Object(map) = file else {
                return false;
            };
            map.get("path").and_then(Value::as_string) == Some("src/main.rs")
                && matches!(map.get("size"), Some(Value::Number(_)))
        }));
    }

    #[test]
    fn workspace_exists_requires_permission() {
        let mut executor = executor();

        let err = executor
            .eval_call(FunctionCall {
                name: vec!["workspace".into(), "exists".into()],
                args: vec![Expr::String("Cargo.toml".into())],
                config: None,
            })
            .unwrap_err();

        assert!(err.contains("workspace.read"));
    }

    #[test]
    fn workspace_exists_reports_workspace_paths() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&vec![(
            "workspace".into(),
            "read".into(),
        )]));

        let exists = executor
            .eval_call(FunctionCall {
                name: vec!["workspace".into(), "exists".into()],
                args: vec![Expr::String("Cargo.toml".into())],
                config: None,
            })
            .unwrap();

        let missing = executor
            .eval_call(FunctionCall {
                name: vec!["workspace".into(), "exists".into()],
                args: vec![Expr::String("definitely-missing.file".into())],
                config: None,
            })
            .unwrap();

        assert!(matches!(exists, Value::Bool(true)));
        assert!(matches!(missing, Value::Bool(false)));
    }

    #[test]
    fn workspace_read_reads_text_file() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&vec![(
            "workspace".into(),
            "read".into(),
        )]));

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["workspace".into(), "read".into()],
                args: vec![Expr::String("Cargo.toml".into())],
                config: None,
            })
            .unwrap();

        assert!(result
            .as_string()
            .expect("Expected Cargo.toml text")
            .contains("[package]"));
    }

    #[test]
    fn workspace_read_rejects_parent_traversal() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&vec![(
            "workspace".into(),
            "read".into(),
        )]));

        let err = executor
            .eval_call(FunctionCall {
                name: vec!["workspace".into(), "read".into()],
                args: vec![Expr::String("../Cargo.toml".into())],
                config: None,
            })
            .unwrap_err();

        assert!(err.contains("outside the workspace"));
    }

    #[test]
    fn workspace_files_lists_files_in_directory() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&vec![(
            "workspace".into(),
            "read".into(),
        )]));

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["workspace".into(), "files".into()],
                args: vec![Expr::String("src".into())],
                config: None,
            })
            .unwrap();

        let Value::List(files) = result else {
            panic!("Expected file list");
        };

        assert!(files.iter().any(|file| {
            let Value::Object(map) = file else {
                return false;
            };
            map.get("path").and_then(Value::as_string) == Some("src/main.rs")
                && map.get("name").and_then(Value::as_string) == Some("main.rs")
        }));
    }

    #[test]
    fn workspace_dirs_lists_directories_in_directory() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&vec![(
            "workspace".into(),
            "read".into(),
        )]));

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["workspace".into(), "dirs".into()],
                args: Vec::new(),
                config: None,
            })
            .unwrap();

        let Value::List(dirs) = result else {
            panic!("Expected directory list");
        };

        assert!(dirs.iter().any(|dir| {
            let Value::Object(map) = dir else {
                return false;
            };
            map.get("path").and_then(Value::as_string) == Some("src")
                && map.get("name").and_then(Value::as_string) == Some("src")
        }));
    }

    #[test]
    fn workspace_env_requires_permission() {
        let mut executor = executor();

        let err = executor
            .eval_call(FunctionCall {
                name: vec!["workspace".into(), "env".into()],
                args: vec![Expr::String("DATABASE_URL".into())],
                config: None,
            })
            .unwrap_err();

        assert!(err.contains("workspace.env"));
    }

    #[test]
    fn workspace_env_reads_environment_variable() {
        let name = format!("ZEN_WORKSPACE_TEST_{}", std::process::id());
        env::set_var(&name, "postgres://localhost/app");
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&vec![(
            "workspace".into(),
            "env".into(),
        )]));

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["workspace".into(), "env".into()],
                args: vec![Expr::String(name.clone())],
                config: None,
            })
            .unwrap();

        env::remove_var(name);
        assert_eq!(result.as_string(), Some("postgres://localhost/app"));
    }

    #[test]
    fn state_save_requires_permission() {
        let mut executor = executor_with_state_workspace(Vec::new());
        executor
            .ctx
            .vars
            .insert("name".into(), Value::String("zen".into()));

        let err = executor
            .eval_call(FunctionCall {
                name: vec!["state".into(), "save".into()],
                args: Vec::new(),
                config: None,
            })
            .unwrap_err();

        assert!(err.contains("state.write"));
    }

    #[test]
    fn state_load_requires_permission() {
        let mut executor = executor_with_state_workspace(Vec::new());

        let err = executor
            .eval_call(FunctionCall {
                name: vec!["state".into(), "load".into()],
                args: Vec::new(),
                config: None,
            })
            .unwrap_err();

        assert!(err.contains("state.read"));
    }

    #[test]
    fn state_save_and_load_roundtrip_session_variables() {
        let permissions = vec![
            ("state".into(), "write".into()),
            ("state".into(), "read".into()),
        ];
        let mut writer = executor_with_state_workspace(permissions.clone());
        writer
            .ctx
            .vars
            .insert("name".into(), Value::String("zen".into()));
        writer.ctx.vars.insert("count".into(), Value::Number(3.0));

        let saved = writer
            .eval_call(FunctionCall {
                name: vec!["state".into(), "save".into()],
                args: Vec::new(),
                config: None,
            })
            .unwrap();

        assert_eq!(
            match saved {
                Value::Object(map) => map.get("count").and_then(Value::as_number),
                _ => None,
            },
            Some(2.0)
        );

        let mut loader = Executor::new_with_permissions(PermissionSet::new(&permissions));
        loader.workspace_root = writer.workspace_root.clone();
        loader.cwd = writer.cwd.clone();

        let loaded = loader
            .eval_call(FunctionCall {
                name: vec!["state".into(), "load".into()],
                args: Vec::new(),
                config: None,
            })
            .unwrap();

        assert_eq!(
            match loaded {
                Value::Object(map) => map.get("count").and_then(Value::as_number),
                _ => None,
            },
            Some(2.0)
        );
        assert_eq!(
            loader.ctx.vars.get("name").and_then(Value::as_string),
            Some("zen")
        );
        assert_eq!(
            loader.ctx.vars.get("count").and_then(Value::as_number),
            Some(3.0)
        );
        assert_eq!(
            loader.env.get("name").and_then(Value::as_string),
            Some("zen")
        );
    }

    #[test]
    fn state_clear_requires_permission() {
        let mut executor = executor_with_state_workspace(Vec::new());

        let err = executor
            .eval_call(FunctionCall {
                name: vec!["state".into(), "clear".into()],
                args: Vec::new(),
                config: None,
            })
            .unwrap_err();

        assert!(err.contains("state.write"));
    }

    #[test]
    fn state_clear_deletes_saved_state_without_clearing_live_variables() {
        let permissions = vec![("state".into(), "write".into())];
        let mut executor = executor_with_state_workspace(permissions);
        executor
            .ctx
            .vars
            .insert("name".into(), Value::String("zen".into()));

        executor
            .eval_call(FunctionCall {
                name: vec!["state".into(), "save".into()],
                args: Vec::new(),
                config: None,
            })
            .unwrap();

        assert!(executor.state_path().is_file());

        let cleared = executor
            .eval_call(FunctionCall {
                name: vec!["state".into(), "clear".into()],
                args: Vec::new(),
                config: None,
            })
            .unwrap();

        let Value::Object(map) = cleared else {
            panic!("Expected clear result");
        };

        assert!(matches!(map.get("deleted"), Some(Value::Bool(true))));
        assert!(!executor.state_path().exists());
        assert_eq!(
            executor.ctx.vars.get("name").and_then(Value::as_string),
            Some("zen")
        );
    }

    #[test]
    fn state_clear_reports_false_when_no_saved_state_exists() {
        let mut executor = executor_with_state_workspace(vec![("state".into(), "write".into())]);

        let cleared = executor
            .eval_call(FunctionCall {
                name: vec!["state".into(), "clear".into()],
                args: Vec::new(),
                config: None,
            })
            .unwrap();

        let Value::Object(map) = cleared else {
            panic!("Expected clear result");
        };

        assert!(matches!(map.get("deleted"), Some(Value::Bool(false))));
    }

    #[test]
    fn state_list_reports_current_variables_sorted() {
        let mut executor = executor_with_state_workspace(Vec::new());
        executor.ctx.vars.insert("zeta".into(), Value::Bool(true));
        executor
            .ctx
            .vars
            .insert("alpha".into(), Value::String("first".into()));

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["state".into(), "list".into()],
                args: Vec::new(),
                config: None,
            })
            .unwrap();

        let Value::List(items) = result else {
            panic!("Expected state list");
        };

        let names: Vec<_> = items
            .iter()
            .map(|item| match item {
                Value::Object(map) => map.get("name").and_then(Value::as_string).unwrap(),
                _ => panic!("Expected state entry"),
            })
            .collect();

        assert_eq!(names, vec!["alpha", "zeta"]);
    }

    #[test]
    fn time_freeze_freezes_now_unix_and_millis() {
        let mut executor = executor();

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["time".into(), "freeze".into()],
                args: vec![Expr::String("2026-05-14T12:34:56Z".into())],
                config: None,
            })
            .unwrap();

        match result {
            Value::String(value) => assert_eq!(value, "2026-05-14T12:34:56Z"),
            other => panic!("Expected frozen timestamp, got {:?}", other),
        }

        match executor
            .time_value(vec![Expr::Ident("now".into())], Value::Null)
            .unwrap()
        {
            Value::String(value) => assert_eq!(value, "2026-05-14T12:34:56Z"),
            other => panic!("Expected frozen now string, got {:?}", other),
        }

        match executor
            .time_value(vec![Expr::Ident("unix".into())], Value::Null)
            .unwrap()
        {
            Value::Number(value) => assert_eq!(value, 1_778_762_096.0),
            other => panic!("Expected frozen unix number, got {:?}", other),
        }

        match executor
            .time_value(vec![Expr::Ident("millis".into())], Value::Null)
            .unwrap()
        {
            Value::Number(value) => assert_eq!(value, 1_778_762_096_000.0),
            other => panic!("Expected frozen millis number, got {:?}", other),
        }
    }

    #[test]
    fn time_mock_alias_still_freezes_time() {
        let mut executor = executor();

        let result = executor
            .time_value(
                vec![
                    Expr::Ident("mock".into()),
                    Expr::String("2026-05-14T12:34:56Z".into()),
                ],
                Value::Null,
            )
            .unwrap();

        match result {
            Value::String(value) => assert_eq!(value, "2026-05-14T12:34:56Z"),
            other => panic!("Expected frozen timestamp, got {:?}", other),
        }

        match executor
            .time_value(vec![Expr::Ident("now".into())], Value::Null)
            .unwrap()
        {
            Value::String(value) => assert_eq!(value, "2026-05-14T12:34:56Z"),
            other => panic!("Expected frozen now string, got {:?}", other),
        }
    }

    #[test]
    fn time_freeze_drives_stamp_and_format() {
        let mut executor = executor();

        executor
            .time_value(
                vec![
                    Expr::Ident("freeze".into()),
                    Expr::String("2026-05-14T12:34:56Z".into()),
                ],
                Value::Null,
            )
            .unwrap();

        let expected_year = Utc
            .timestamp_opt(1_778_762_096, 0)
            .single()
            .unwrap()
            .with_timezone(&Local)
            .format("%Y")
            .to_string();

        match executor
            .time_value(
                vec![Expr::Ident("format".into()), Expr::String("%Y".into())],
                Value::Null,
            )
            .unwrap()
        {
            Value::String(value) => assert_eq!(value, expected_year),
            other => panic!("Expected frozen formatted time, got {:?}", other),
        }

        let mut item = HashMap::new();
        item.insert("name".into(), Value::String("ada".into()));

        match executor
            .time_value(
                vec![Expr::Ident("stamp".into()), Expr::String("seen_at".into())],
                Value::Object(item),
            )
            .unwrap()
        {
            Value::Object(map) => match map.get("seen_at") {
                Some(Value::String(value)) => assert_eq!(value, "2026-05-14T12:34:56Z"),
                other => panic!("Expected stamped timestamp, got {:?}", other),
            },
            other => panic!("Expected stamped object, got {:?}", other),
        }
    }

    #[test]
    fn time_local_uses_local_timezone() {
        let mut executor = executor();

        executor
            .time_value(
                vec![
                    Expr::Ident("freeze".into()),
                    Expr::String("2026-05-14T12:34:56Z".into()),
                ],
                Value::Null,
            )
            .unwrap();

        let expected = Utc
            .timestamp_opt(1_778_762_096, 0)
            .single()
            .unwrap()
            .with_timezone(&Local)
            .format("%Y-%m-%dT%H:%M:%S%:z")
            .to_string();

        match executor
            .time_value(vec![Expr::Ident("local".into())], Value::Null)
            .unwrap()
        {
            Value::String(value) => assert_eq!(value, expected),
            other => panic!("Expected local frozen time, got {:?}", other),
        }

        let expected_year = Utc
            .timestamp_opt(1_778_762_096, 0)
            .single()
            .unwrap()
            .with_timezone(&Local)
            .format("%Y")
            .to_string();

        match executor
            .time_value(
                vec![
                    Expr::Ident("local".into()),
                    Expr::Ident("format".into()),
                    Expr::String("%Y".into()),
                ],
                Value::Null,
            )
            .unwrap()
        {
            Value::String(value) => assert_eq!(value, expected_year),
            other => panic!("Expected local formatted time, got {:?}", other),
        }
    }

    #[test]
    fn time_local_dotted_forms_work() {
        let mut executor = executor();

        executor
            .eval_call(FunctionCall {
                name: vec!["time".into(), "freeze".into()],
                args: vec![Expr::String("2026-05-14T12:34:56Z".into())],
                config: None,
            })
            .unwrap();

        let expected = Utc
            .timestamp_opt(1_778_762_096, 0)
            .single()
            .unwrap()
            .with_timezone(&Local)
            .format("%Y-%m-%dT%H:%M:%S%:z")
            .to_string();

        match executor
            .eval_call(FunctionCall {
                name: vec!["time".into(), "local".into()],
                args: Vec::new(),
                config: None,
            })
            .unwrap()
        {
            Value::String(value) => assert_eq!(value, expected),
            other => panic!("Expected local dotted time, got {:?}", other),
        }

        match executor
            .eval_call(FunctionCall {
                name: vec!["time".into(), "local".into(), "format".into()],
                args: vec![Expr::String("%Y".into())],
                config: None,
            })
            .unwrap()
        {
            Value::String(value) => assert_eq!(value, "2026"),
            other => panic!("Expected local dotted formatted time, got {:?}", other),
        }
    }

    #[test]
    fn measure_wraps_call_with_monotonic_duration_and_clock_labels() {
        let mut executor = executor();

        executor
            .eval_call(FunctionCall {
                name: vec!["time".into(), "freeze".into()],
                args: vec![Expr::String("2026-05-14T12:34:56Z".into())],
                config: None,
            })
            .unwrap();

        let measured = executor
            .eval_call(FunctionCall {
                name: vec!["measure".into()],
                args: vec![Expr::Call(FunctionCall {
                    name: vec!["time".into(), "now".into()],
                    args: Vec::new(),
                    config: None,
                })],
                config: None,
            })
            .unwrap();

        let Value::Object(map) = measured else {
            panic!("Expected measured object");
        };

        match map.get("success") {
            Some(Value::Bool(value)) => assert!(*value),
            other => panic!("Expected success boolean, got {:?}", other),
        }

        match map.get("duration_ms") {
            Some(Value::Number(value)) => assert!(*value >= 0.0),
            other => panic!("Expected duration_ms number, got {:?}", other),
        }

        match map.get("started_at") {
            Some(Value::String(value)) => assert_eq!(value, "2026-05-14T12:34:56Z"),
            other => panic!("Expected started_at timestamp, got {:?}", other),
        }

        match map.get("ended_at") {
            Some(Value::String(value)) => assert_eq!(value, "2026-05-14T12:34:56Z"),
            other => panic!("Expected ended_at timestamp, got {:?}", other),
        }

        match map.get("result") {
            Some(Value::String(value)) => assert_eq!(value, "2026-05-14T12:34:56Z"),
            other => panic!("Expected measured result, got {:?}", other),
        }
    }

    #[test]
    fn measure_can_build_command_style_call_from_args() {
        let mut executor = executor();

        let measured = executor
            .eval_call(FunctionCall {
                name: vec!["measure".into()],
                args: vec![Expr::Ident("time".into()), Expr::Ident("unix".into())],
                config: None,
            })
            .unwrap();

        let Value::Object(map) = measured else {
            panic!("Expected measured object");
        };

        assert!(matches!(map.get("result"), Some(Value::Number(_))));
    }

    #[test]
    fn measure_output_can_be_selected_in_pipeline() {
        let mut executor = executor();

        executor
            .eval_call(FunctionCall {
                name: vec!["time".into(), "freeze".into()],
                args: vec![Expr::String("2026-05-14T12:34:56Z".into())],
                config: None,
            })
            .unwrap();

        let pipeline = Pipeline {
            base: Expr::Call(FunctionCall {
                name: vec!["measure".into()],
                args: vec![Expr::Call(FunctionCall {
                    name: vec!["time".into(), "now".into()],
                    args: Vec::new(),
                    config: None,
                })],
                config: None,
            }),
            stages: vec![PipeStage::Select {
                fields: vec!["duration_ms".into(), "result".into()],
            }],
        };

        let selected = executor.eval_pipeline(pipeline).unwrap();
        let Value::Object(map) = selected else {
            panic!("Expected selected measure object");
        };

        assert!(matches!(map.get("duration_ms"), Some(Value::Number(_))));
        assert_eq!(
            map.get("result").and_then(Value::as_string),
            Some("2026-05-14T12:34:56Z")
        );
        assert!(!map.contains_key("started_at"));
        assert!(!map.contains_key("ended_at"));
    }

    #[test]
    fn assignment_can_capture_pipeline_result() {
        let mut executor = executor();

        executor
            .eval_call(FunctionCall {
                name: vec!["time".into(), "freeze".into()],
                args: vec![Expr::String("2026-05-14T12:34:56Z".into())],
                config: None,
            })
            .unwrap();

        let result = executor
            .execute_capture(parse("let t = time.now | time.format \"%I:%M:%S %p\"\n"))
            .unwrap();
        let expected = Utc
            .timestamp_opt(1_778_762_096, 0)
            .single()
            .unwrap()
            .with_timezone(&Local)
            .format("%I:%M:%S %p")
            .to_string();

        assert_eq!(result.as_string(), Some(expected.as_str()));
        assert_eq!(
            executor.ctx.vars.get("t").and_then(Value::as_string),
            Some(expected.as_str())
        );
    }

    #[test]
    fn benchmark_sleep_returns_summary() {
        let mut executor = executor();

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["benchmark".into()],
                args: vec![
                    Expr::Number(3.0),
                    Expr::Ident("sleep".into()),
                    Expr::Number(1.0),
                    Expr::Ident("ms".into()),
                ],
                config: None,
            })
            .unwrap();

        let Value::Object(map) = result else {
            panic!("Expected benchmark object");
        };

        assert_eq!(map.get("runs").and_then(Value::as_number), Some(3.0));
        assert_eq!(map.get("failures").and_then(Value::as_number), Some(0.0));
        assert!(matches!(map.get("success"), Some(Value::Bool(true))));
        assert!(matches!(map.get("min_ms"), Some(Value::Number(_))));
        assert!(matches!(map.get("avg_ms"), Some(Value::Number(_))));
        assert!(matches!(map.get("median_ms"), Some(Value::Number(_))));
        assert!(matches!(map.get("max_ms"), Some(Value::Number(_))));
    }

    #[test]
    fn benchmark_output_can_be_selected_in_pipeline() {
        let mut executor = executor();

        let pipeline = Pipeline {
            base: Expr::Call(FunctionCall {
                name: vec!["benchmark".into()],
                args: vec![
                    Expr::Number(2.0),
                    Expr::Ident("sleep".into()),
                    Expr::Number(1.0),
                    Expr::Ident("ms".into()),
                ],
                config: None,
            }),
            stages: vec![PipeStage::Select {
                fields: vec![
                    "runs".into(),
                    "min_ms".into(),
                    "avg_ms".into(),
                    "max_ms".into(),
                ],
            }],
        };

        let selected = executor.eval_pipeline(pipeline).unwrap();
        let Value::Object(map) = selected else {
            panic!("Expected selected benchmark object");
        };

        assert_eq!(map.get("runs").and_then(Value::as_number), Some(2.0));
        assert!(map.contains_key("min_ms"));
        assert!(map.contains_key("avg_ms"));
        assert!(map.contains_key("max_ms"));
        assert!(!map.contains_key("median_ms"));
    }

    #[test]
    fn fields_filters_object_fields() {
        let executor = executor();
        let mut map = HashMap::new();
        map.insert("name".into(), Value::String("Cargo.toml".into()));
        map.insert("path".into(), Value::String("Cargo.toml".into()));
        map.insert("size".into(), Value::Number(950.0));

        let selected = executor
            .pipe_fields(Value::Object(map), vec!["name".into(), "missing".into()])
            .unwrap();

        let Value::Object(map) = selected else {
            panic!("Expected selected object");
        };
        assert_eq!(
            map.get("name").and_then(Value::as_string),
            Some("Cargo.toml")
        );
        assert!(!map.contains_key("missing"));
        assert!(!map.contains_key("path"));
    }

    #[test]
    fn fields_filters_list_of_objects() {
        let executor = executor();
        let mut first = HashMap::new();
        first.insert("name".into(), Value::String("Cargo.toml".into()));
        first.insert("path".into(), Value::String("Cargo.toml".into()));
        first.insert("size".into(), Value::Number(950.0));

        let selected = executor
            .pipe_fields(
                Value::List(vec![Value::Object(first)]),
                vec!["name".into(), "path".into()],
            )
            .unwrap();

        let Value::List(items) = selected else {
            panic!("Expected selected list");
        };
        let Value::Object(map) = &items[0] else {
            panic!("Expected selected object");
        };
        assert_eq!(map.len(), 2);
        assert_eq!(
            map.get("name").and_then(Value::as_string),
            Some("Cargo.toml")
        );
        assert_eq!(
            map.get("path").and_then(Value::as_string),
            Some("Cargo.toml")
        );
    }

    #[test]
    fn fields_rejects_non_object_list_items() {
        let executor = executor();

        let err = executor
            .pipe_fields(
                Value::List(vec![Value::String("not-object".into())]),
                vec!["name".into()],
            )
            .unwrap_err();

        assert_eq!(err, "fields expects an object or list of objects");
    }

    #[test]
    fn get_extracts_object_field() {
        let executor = executor();
        let mut map = HashMap::new();
        map.insert("stdout".into(), Value::String("hello".into()));
        map.insert("success".into(), Value::Bool(true));

        let value = executor
            .pipe_get(Value::Object(map), "stdout".into())
            .unwrap();

        assert_eq!(value.as_string(), Some("hello"));
    }

    #[test]
    fn where_filters_boolean_equality() {
        let executor = executor();
        let input = Value::List(vec![
            exec_like_entry("ok", true),
            exec_like_entry("failed", false),
        ]);

        let filtered = executor
            .pipe_where(
                input,
                Expr::Binary {
                    left: Box::new(Expr::Ident("success".into())),
                    op: BinOp::Eq,
                    right: Box::new(Expr::Bool(true)),
                },
            )
            .unwrap();

        let Value::List(items) = filtered else {
            panic!("Expected filtered list");
        };
        assert_eq!(items.len(), 1);
        let Value::Object(map) = &items[0] else {
            panic!("Expected object");
        };
        assert_eq!(map.get("stdout").and_then(Value::as_string), Some("ok"));
    }

    #[test]
    fn json_pipeline_stages_roundtrip() {
        let executor = executor();
        let mut map = HashMap::new();
        map.insert("name".into(), Value::String("zen".into()));
        map.insert("success".into(), Value::Bool(true));

        let json = executor.pipe_to_json(Value::Object(map)).unwrap();
        assert!(json.as_string().unwrap().contains("\"name\""));

        let parsed = executor.pipe_from_json(json).unwrap();
        let Value::Object(map) = parsed else {
            panic!("Expected parsed object");
        };
        assert_eq!(map.get("name").and_then(Value::as_string), Some("zen"));
        assert!(matches!(map.get("success"), Some(Value::Bool(true))));
    }

    #[test]
    fn save_writes_pipeline_input_to_workspace_file() {
        let executor = executor_with_state_workspace(vec![("fs".into(), "write".into())]);
        let saved = executor
            .pipe_save(Value::String("{\"ok\":true}".into()), "result.json".into())
            .unwrap();

        let Value::Object(map) = saved else {
            panic!("Expected save result object");
        };
        assert!(matches!(map.get("saved"), Some(Value::Bool(true))));
        let written = fs::read_to_string(executor.workspace_root.join("result.json")).unwrap();
        assert_eq!(written, "{\"ok\":true}");
    }

    #[test]
    fn where_filters_string_equality() {
        let executor = executor();
        let input = Value::List(vec![
            plugin_inventory_entry("core", "builtin"),
            plugin_inventory_entry("hello", "external"),
        ]);

        let filtered = executor
            .pipe_where(
                input,
                Expr::Binary {
                    left: Box::new(Expr::Ident("kind".into())),
                    op: BinOp::Eq,
                    right: Box::new(Expr::String("external".into())),
                },
            )
            .unwrap();

        let Value::List(items) = filtered else {
            panic!("Expected filtered list");
        };
        assert_eq!(items.len(), 1);
        let Value::Object(map) = &items[0] else {
            panic!("Expected object");
        };
        assert_eq!(map.get("name").and_then(Value::as_string), Some("hello"));
    }

    #[test]
    fn where_filters_string_inequality() {
        let executor = executor();
        let input = Value::List(vec![
            plugin_inventory_entry("core", "builtin"),
            plugin_inventory_entry("hello", "external"),
        ]);

        let filtered = executor
            .pipe_where(
                input,
                Expr::Binary {
                    left: Box::new(Expr::Ident("kind".into())),
                    op: BinOp::Neq,
                    right: Box::new(Expr::String("external".into())),
                },
            )
            .unwrap();

        let Value::List(items) = filtered else {
            panic!("Expected filtered list");
        };
        assert_eq!(items.len(), 1);
        let Value::Object(map) = &items[0] else {
            panic!("Expected object");
        };
        assert_eq!(map.get("name").and_then(Value::as_string), Some("core"));
    }

    #[test]
    fn sleep_returns_pipeline_input() {
        let mut executor = executor();

        let result = executor
            .eval_call_with_input(
                FunctionCall {
                    name: vec!["sleep".into()],
                    args: vec![Expr::Ident("1ms".into())],
                    config: None,
                },
                Value::String("ready".into()),
            )
            .unwrap();

        assert_eq!(result.as_string(), Some("ready"));
    }

    #[test]
    fn sleep_can_be_measured() {
        let mut executor = executor();

        let measured = executor
            .eval_call(FunctionCall {
                name: vec!["measure".into()],
                args: vec![Expr::Ident("sleep".into()), Expr::Ident("10ms".into())],
                config: None,
            })
            .unwrap();

        let Value::Object(map) = measured else {
            panic!("Expected measured sleep object");
        };

        match map.get("duration_ms") {
            Some(Value::Number(value)) => assert!(
                *value >= 8.0,
                "Expected sleep measurement near requested duration, got {value}ms"
            ),
            other => panic!("Expected duration_ms number, got {:?}", other),
        }

        assert!(matches!(map.get("result"), Some(Value::Null)));
    }

    #[test]
    fn sleep_duration_supports_jitter() {
        let executor = executor();

        let duration = executor
            .sleep_duration_from_parts(&[
                "10".into(),
                "ms".into(),
                "jitter".into(),
                "5".into(),
                "ms".into(),
            ])
            .unwrap();

        assert!(duration >= Duration::from_millis(10));
        assert!(duration <= Duration::from_millis(15));
    }

    #[test]
    fn sleep_jitter_only_is_bounded() {
        let executor = executor();

        let duration = executor
            .sleep_duration_from_parts(&["jitter".into(), "5".into(), "ms".into()])
            .unwrap();

        assert!(duration <= Duration::from_millis(5));
    }

    #[test]
    fn sleep_until_past_returns_zero_duration() {
        let executor = executor();

        let duration = executor
            .sleep_duration_from_parts(&["until".into(), "1970-01-01T00:00:00Z".into()])
            .unwrap();

        assert_eq!(duration, Duration::ZERO);
    }

    #[test]
    fn time_since_returns_duration_summary() {
        let mut executor = executor();

        executor
            .eval_call(FunctionCall {
                name: vec!["time".into(), "freeze".into()],
                args: vec![Expr::String("2026-05-14T00:00:00Z".into())],
                config: None,
            })
            .unwrap();

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["time".into(), "since".into()],
                args: vec![Expr::String("2026-05-01".into())],
                config: None,
            })
            .unwrap();

        let Value::Object(map) = result else {
            panic!("Expected time.since object");
        };

        assert_eq!(
            map.get("seconds").and_then(Value::as_number),
            Some(1_123_200.0)
        );
        assert_eq!(map.get("days").and_then(Value::as_number), Some(13.0));
        assert_eq!(map.get("human").and_then(Value::as_string), Some("13 days"));
    }

    #[test]
    fn time_until_returns_duration_summary() {
        let mut executor = executor();

        executor
            .eval_call(FunctionCall {
                name: vec!["time".into(), "freeze".into()],
                args: vec![Expr::String("2026-05-14T00:00:00Z".into())],
                config: None,
            })
            .unwrap();

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["time".into(), "until".into()],
                args: vec![Expr::String("2026-05-15T12:00:00Z".into())],
                config: None,
            })
            .unwrap();

        let Value::Object(map) = result else {
            panic!("Expected time.until object");
        };

        assert_eq!(
            map.get("seconds").and_then(Value::as_number),
            Some(129_600.0)
        );
        assert_eq!(map.get("hours").and_then(Value::as_number), Some(36.0));
        assert_eq!(map.get("human").and_then(Value::as_string), Some("1 day"));
    }

    #[test]
    fn time_parse_handles_named_phrases() {
        let mut executor = executor();

        executor
            .eval_call(FunctionCall {
                name: vec!["time".into(), "freeze".into()],
                args: vec![Expr::String("2026-05-14T12:34:56Z".into())],
                config: None,
            })
            .unwrap();

        let cases = [
            ("now", "2026-05-14T12:34:56Z"),
            ("today", "2026-05-14T00:00:00Z"),
            ("tomorrow", "2026-05-15T00:00:00Z"),
            ("yesterday", "2026-05-13T00:00:00Z"),
        ];

        for (phrase, expected) in cases {
            match executor
                .eval_call(FunctionCall {
                    name: vec!["time".into(), "parse".into()],
                    args: vec![Expr::String(phrase.into())],
                    config: None,
                })
                .unwrap()
            {
                Value::String(value) => assert_eq!(value, expected),
                other => panic!("Expected parsed timestamp, got {:?}", other),
            }
        }
    }

    #[test]
    fn time_timestamp_alias_matches_now() {
        let mut executor = executor();

        executor
            .eval_call(FunctionCall {
                name: vec!["time".into(), "freeze".into()],
                args: vec![Expr::String("2026-05-14T12:34:56Z".into())],
                config: None,
            })
            .unwrap();

        match executor
            .eval_call(FunctionCall {
                name: vec!["time".into(), "timestamp".into()],
                args: Vec::new(),
                config: None,
            })
            .unwrap()
        {
            Value::String(value) => assert_eq!(value, "2026-05-14T12:34:56Z"),
            other => panic!("Expected timestamp string, got {:?}", other),
        }
    }

    #[test]
    fn time_parse_handles_relative_phrases() {
        let mut executor = executor();

        executor
            .eval_call(FunctionCall {
                name: vec!["time".into(), "freeze".into()],
                args: vec![Expr::String("2026-05-14T12:34:56Z".into())],
                config: None,
            })
            .unwrap();

        let cases = [
            ("in 3 days", "2026-05-17T12:34:56Z"),
            ("2 hours ago", "2026-05-14T10:34:56Z"),
            ("in 500 ms", "2026-05-14T12:34:56Z"),
        ];

        for (phrase, expected) in cases {
            match executor
                .eval_call(FunctionCall {
                    name: vec!["time".into(), "parse".into()],
                    args: vec![Expr::String(phrase.into())],
                    config: None,
                })
                .unwrap()
            {
                Value::String(value) => assert_eq!(value, expected),
                other => panic!("Expected parsed timestamp, got {:?}", other),
            }
        }
    }

    #[test]
    fn time_since_accepts_phrases() {
        let mut executor = executor();

        executor
            .eval_call(FunctionCall {
                name: vec!["time".into(), "freeze".into()],
                args: vec![Expr::String("2026-05-14T12:00:00Z".into())],
                config: None,
            })
            .unwrap();

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["time".into(), "since".into()],
                args: vec![Expr::String("yesterday".into())],
                config: None,
            })
            .unwrap();

        let Value::Object(map) = result else {
            panic!("Expected time.since object");
        };

        assert_eq!(map.get("hours").and_then(Value::as_number), Some(36.0));
    }

    #[test]
    fn sleep_until_accepts_past_phrases() {
        let mut executor = executor();

        executor
            .eval_call(FunctionCall {
                name: vec!["time".into(), "freeze".into()],
                args: vec![Expr::String("2026-05-14T12:34:56Z".into())],
                config: None,
            })
            .unwrap();

        let duration = executor
            .sleep_duration_from_parts(&["until".into(), "yesterday".into()])
            .unwrap();

        assert_eq!(duration, Duration::ZERO);
    }

    #[test]
    fn time_since_can_be_selected_in_pipeline() {
        let mut executor = executor();

        executor
            .eval_call(FunctionCall {
                name: vec!["time".into(), "freeze".into()],
                args: vec![Expr::String("2026-05-14T00:00:00Z".into())],
                config: None,
            })
            .unwrap();

        let pipeline = Pipeline {
            base: Expr::Call(FunctionCall {
                name: vec!["time".into(), "since".into()],
                args: vec![Expr::String("2026-05-01".into())],
                config: None,
            }),
            stages: vec![PipeStage::Select {
                fields: vec!["days".into(), "human".into()],
            }],
        };

        let selected = executor.eval_pipeline(pipeline).unwrap();
        let Value::Object(map) = selected else {
            panic!("Expected selected time.since object");
        };

        assert_eq!(map.get("days").and_then(Value::as_number), Some(13.0));
        assert_eq!(map.get("human").and_then(Value::as_string), Some("13 days"));
        assert!(!map.contains_key("seconds"));
    }

    #[test]
    fn measure_exec_waits_for_command_completion() {
        let _guard = crate::interrupt::lock_for_test();
        crate::interrupt::clear_interrupt();
        let mut executor = executor_with_exec_permission();
        let command = if cfg!(windows) {
            vec![
                Expr::Ident("exec".into()),
                Expr::Ident("powershell".into()),
                Expr::String("-NoProfile".into()),
                Expr::String("-Command".into()),
                Expr::String("Start-Sleep -Milliseconds 100".into()),
            ]
        } else {
            vec![
                Expr::Ident("exec".into()),
                Expr::Ident("sh".into()),
                Expr::String("-c".into()),
                Expr::String("sleep 0.1".into()),
            ]
        };

        let measured = executor
            .eval_call(FunctionCall {
                name: vec!["measure".into()],
                args: command,
                config: None,
            })
            .unwrap();

        let Value::Object(map) = measured else {
            panic!("Expected measured object");
        };

        match map.get("duration_ms") {
            Some(Value::Number(value)) => assert!(
                *value >= 80.0,
                "Expected measure exec to wait for completion, got {value}ms"
            ),
            other => panic!("Expected duration_ms number, got {:?}", other),
        }
    }

    #[test]
    fn exec_request_parses_wait_children_option() {
        let mut executor = executor_with_exec_permission();

        let request = executor
            .exec_request_from_call(FunctionCall {
                name: vec!["exec".into()],
                args: vec![
                    Expr::Ident("tool".into()),
                    Expr::Ident("wait".into()),
                    Expr::Ident("children".into()),
                    Expr::Ident("timeout".into()),
                    Expr::Ident("1s".into()),
                ],
                config: None,
            })
            .unwrap();

        assert_eq!(request.command, "tool");
        assert_eq!(request.argv, Some(vec!["tool".into()]));
        assert!(request.wait_children);
        assert_eq!(request.timeout, Some(Duration::from_secs(1)));
    }

    #[test]
    fn exec_request_parses_workdir_option() {
        let mut executor = executor_with_exec_permission();

        let request = executor
            .exec_request_from_call(FunctionCall {
                name: vec!["exec".into()],
                args: vec![
                    Expr::Ident("tool".into()),
                    Expr::Ident("workdir".into()),
                    Expr::String("src".into()),
                ],
                config: None,
            })
            .unwrap();

        assert_eq!(request.command, "tool");
        assert_eq!(request.argv, Some(vec!["tool".into()]));
        assert!(request
            .workdir
            .as_deref()
            .unwrap()
            .replace('\\', "/")
            .ends_with("/src"));
    }

    #[test]
    fn pwd_reports_session_workdir() {
        let mut executor = executor();

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["pwd".into()],
                args: Vec::new(),
                config: None,
            })
            .unwrap();

        match result {
            Value::String(value) => assert!(!value.is_empty()),
            other => panic!("Expected pwd string, got {:?}", other),
        }
    }

    #[test]
    fn cd_changes_session_workdir_and_default_exec_workdir() {
        let mut executor = executor_with_exec_permission();

        executor
            .eval_call(FunctionCall {
                name: vec!["cd".into()],
                args: vec![Expr::String("src".into())],
                config: None,
            })
            .unwrap();

        let pwd = executor
            .eval_call(FunctionCall {
                name: vec!["pwd".into()],
                args: Vec::new(),
                config: None,
            })
            .unwrap();

        match pwd {
            Value::String(value) => assert!(value.replace('\\', "/").ends_with("/src")),
            other => panic!("Expected pwd string, got {:?}", other),
        }

        let request = executor
            .exec_request_from_call(FunctionCall {
                name: vec!["exec".into()],
                args: vec![Expr::Ident("tool".into())],
                config: None,
            })
            .unwrap();

        assert!(request
            .workdir
            .as_deref()
            .unwrap()
            .replace('\\', "/")
            .ends_with("/src"));
    }

    #[test]
    fn exec_request_preserves_variable_command_path_as_argv() {
        let mut executor = executor_with_exec_permission();
        executor.ctx.vars.insert(
            "x".into(),
            Value::String("./binn/firewisemail/FirewiseMail.App.exe".into()),
        );

        let request = executor
            .exec_request_from_call(FunctionCall {
                name: vec!["exec".into()],
                args: vec![Expr::Variable("x".into())],
                config: None,
            })
            .unwrap();

        assert_eq!(request.command, "./binn/firewisemail/FirewiseMail.App.exe");
        assert_eq!(
            request.argv,
            Some(vec!["./binn/firewisemail/FirewiseMail.App.exe".into()])
        );
    }

    #[test]
    fn exec_request_preserves_quoted_text_as_single_argv_arg() {
        let mut executor = executor_with_exec_permission();

        let request = executor
            .exec_request_from_call(FunctionCall {
                name: vec!["exec".into()],
                args: vec![
                    Expr::Ident("program".into()),
                    Expr::Ident("with".into()),
                    Expr::String("my app".into()),
                ],
                config: None,
            })
            .unwrap();

        assert_eq!(request.command, "program with \"my app\"");
        assert_eq!(
            request.argv,
            Some(vec!["program".into(), "with".into(), "my app".into()])
        );
    }

    #[test]
    fn which_reports_builtins() {
        let mut executor = executor();

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["which".into()],
                args: vec![Expr::Ident("clear".into())],
                config: None,
            })
            .unwrap();

        assert_eq!(result.as_string(), Some("builtin:clear"));
    }

    #[test]
    fn which_reports_plugin_registered_builtins() {
        let mut executor = executor();

        for command in [
            "fs.list",
            "math.add",
            "process.list",
            "state.save",
            "string.upper",
            "time.format",
            "workspace.root",
            "plugins.list",
        ] {
            let result = executor
                .eval_call(FunctionCall {
                    name: vec!["which".into()],
                    args: vec![Expr::Call(FunctionCall {
                        name: command.split('.').map(str::to_string).collect(),
                        args: Vec::new(),
                        config: None,
                    })],
                    config: None,
                })
                .unwrap();

            assert_eq!(
                result.as_string(),
                Some(format!("builtin:{}", command).as_str())
            );
        }
    }

    #[test]
    fn plugins_list_reports_loaded_plugin_commands() {
        let mut executor = executor();

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["plugins".into(), "list".into()],
                args: Vec::new(),
                config: None,
            })
            .unwrap();

        let Value::List(plugins) = result else {
            panic!("Expected plugins list");
        };

        let core = plugins
            .iter()
            .find_map(|plugin| match plugin {
                Value::Object(map)
                    if map.get("name").and_then(Value::as_string) == Some("core") =>
                {
                    Some(map)
                }
                _ => None,
            })
            .expect("Expected core plugin");

        let Some(Value::List(commands)) = core.get("commands") else {
            panic!("Expected core plugin commands");
        };

        assert!(commands
            .iter()
            .any(|command| command.as_string() == Some("plugins.list")));
    }

    #[test]
    fn plugins_list_reports_plugin_metadata() {
        let mut executor = executor();

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["plugins".into(), "list".into()],
                args: Vec::new(),
                config: None,
            })
            .unwrap();

        let Value::List(plugins) = result else {
            panic!("Expected plugins list");
        };

        let hello = plugins.iter().find_map(|plugin| match plugin {
            Value::Object(map) if map.get("name").and_then(Value::as_string) == Some("hello") => {
                Some(map)
            }
            _ => None,
        });

        if let Some(hello) = hello {
            assert_eq!(
                hello.get("description").and_then(Value::as_string),
                Some("A tiny external plugin that replies with hello.")
            );
            assert_eq!(
                hello.get("version").and_then(Value::as_string),
                Some("0.1.0")
            );
            assert_eq!(
                hello.get("author").and_then(Value::as_string),
                Some("Zen workspace")
            );
        }
    }

    #[test]
    fn plugins_list_reports_command_permissions() {
        let mut executor = executor();

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["plugins".into(), "list".into()],
                args: Vec::new(),
                config: None,
            })
            .unwrap();

        let Value::List(plugins) = result else {
            panic!("Expected plugins list");
        };

        let process = plugins
            .iter()
            .find_map(|plugin| match plugin {
                Value::Object(map)
                    if map.get("name").and_then(Value::as_string) == Some("process") =>
                {
                    Some(map)
                }
                _ => None,
            })
            .expect("Expected process plugin");

        let Some(Value::List(command_permissions)) = process.get("command_permissions") else {
            panic!("Expected process command permissions");
        };

        assert!(command_permissions.iter().any(|entry| {
            let Value::Object(map) = entry else {
                return false;
            };
            map.get("command").and_then(Value::as_string) == Some("exec")
                && map.get("permission").and_then(Value::as_string) == Some("proc.exec")
        }));
    }

    #[test]
    fn plugins_load_and_unload_external_plugin() {
        let mut executor = executor_with_state_workspace(Vec::new());
        let plugin_dir = executor
            .workspace_root
            .join(".zen")
            .join("plugins")
            .join("manual");
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::write(
            plugin_dir.join("plugin.toml"),
            r#"
name = "manual"

[[commands]]
name = "manual.hello"
run = "echo hello"
"#,
        )
        .unwrap();

        let loaded = executor
            .eval_call(FunctionCall {
                name: vec!["plugins".into(), "load".into()],
                args: vec![Expr::String(".zen/plugins/manual".into())],
                config: None,
            })
            .unwrap();
        let Value::Object(loaded_map) = loaded else {
            panic!("Expected plugins.load object");
        };
        assert!(matches!(loaded_map.get("loaded"), Some(Value::Bool(true))));
        assert_eq!(
            loaded_map.get("name").and_then(Value::as_string),
            Some("manual")
        );

        let inventory = executor.plugins_list().unwrap();
        assert!(plugin_inventory_has(&inventory, "manual"));

        let unloaded = executor
            .eval_call(FunctionCall {
                name: vec!["plugins".into(), "unload".into()],
                args: vec![Expr::String("manual".into())],
                config: None,
            })
            .unwrap();
        let Value::Object(unloaded_map) = unloaded else {
            panic!("Expected plugins.unload object");
        };
        assert!(matches!(
            unloaded_map.get("unloaded"),
            Some(Value::Bool(true))
        ));

        let inventory = executor.plugins_list().unwrap();
        assert!(!plugin_inventory_has(&inventory, "manual"));
    }

    #[test]
    fn plugins_discover_reports_loaded_available_and_errors() {
        let mut executor = executor_with_state_workspace(Vec::new());
        let plugins_root = executor.workspace_root.join(".zen").join("plugins");
        let loaded_dir = plugins_root.join("loaded");
        let available_dir = plugins_root.join("available");
        let broken_dir = plugins_root.join("broken");
        fs::create_dir_all(&loaded_dir).unwrap();
        fs::create_dir_all(&available_dir).unwrap();
        fs::create_dir_all(&broken_dir).unwrap();
        fs::write(
            loaded_dir.join("plugin.toml"),
            r#"
name = "loaded"
description = "Loaded plugin."
version = "1.0.0"

[[commands]]
name = "loaded.hello"
run = "echo hello"
"#,
        )
        .unwrap();
        fs::write(
            available_dir.join("plugin.toml"),
            r#"
name = "available"
description = "Available plugin."

[[commands]]
name = "available.hello"
run = "echo hello"
"#,
        )
        .unwrap();
        fs::write(broken_dir.join("plugin.toml"), "name = \"broken\"\n").unwrap();

        executor
            .eval_call(FunctionCall {
                name: vec!["plugins".into(), "load".into()],
                args: vec![Expr::String(".zen/plugins/loaded".into())],
                config: None,
            })
            .unwrap();

        let discovered = executor
            .eval_call(FunctionCall {
                name: vec!["plugins".into(), "discover".into()],
                args: Vec::new(),
                config: None,
            })
            .unwrap();
        let Value::List(entries) = discovered else {
            panic!("Expected discovered plugin list");
        };

        let loaded = plugin_entry(&entries, "loaded").expect("expected loaded plugin");
        assert_eq!(
            loaded.get("status").and_then(Value::as_string),
            Some("loaded")
        );
        assert_eq!(
            loaded.get("description").and_then(Value::as_string),
            Some("Loaded plugin.")
        );
        assert_eq!(
            loaded.get("version").and_then(Value::as_string),
            Some("1.0.0")
        );

        let available = plugin_entry(&entries, "available").expect("expected available plugin");
        assert_eq!(
            available.get("status").and_then(Value::as_string),
            Some("available")
        );

        assert!(entries.iter().any(|entry| {
            let Value::Object(map) = entry else {
                return false;
            };
            map.get("status").and_then(Value::as_string) == Some("error")
                && map
                    .get("error")
                    .and_then(Value::as_string)
                    .is_some_and(|error| {
                        error.contains("manifest must define at least one command")
                    })
        }));
    }

    #[test]
    fn plugins_unload_does_not_remove_builtins() {
        let mut executor = executor();

        let unloaded = executor
            .eval_call(FunctionCall {
                name: vec!["plugins".into(), "unload".into()],
                args: vec![Expr::String("core".into())],
                config: None,
            })
            .unwrap();

        let Value::Object(unloaded_map) = unloaded else {
            panic!("Expected plugins.unload object");
        };
        assert!(matches!(
            unloaded_map.get("unloaded"),
            Some(Value::Bool(false))
        ));

        let inventory = executor.plugins_list().unwrap();
        assert!(plugin_inventory_has(&inventory, "core"));
    }

    #[test]
    fn help_without_args_lists_registered_commands() {
        let mut executor = executor();

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["help".into()],
                args: Vec::new(),
                config: None,
            })
            .unwrap();

        let help = result.as_string().expect("Expected help text");
        assert!(help.contains("Builtin commands"));
        assert!(help.contains("exec"));
        assert!(help.contains("math.add"));
        assert!(help.contains("plugins.list"));
        assert!(help.contains("state.save"));
        assert!(help.contains("string.upper"));
        assert!(help.contains("workspace.root"));
    }

    #[test]
    fn help_for_command_reports_usage_permission_and_examples() {
        let mut executor = executor();

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["help".into()],
                args: vec![Expr::Ident("exec".into())],
                config: None,
            })
            .unwrap();

        let help = result.as_string().expect("Expected help text");
        assert!(help.contains("exec <command>"));
        assert!(help.contains("proc.exec"));
        assert!(help.contains("exec pg_dump --version"));
    }

    #[test]
    fn help_for_dotted_command_reports_plugin_docs() {
        let mut executor = executor();

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["help".into()],
                args: vec![Expr::Call(FunctionCall {
                    name: vec!["time".into(), "format".into()],
                    args: Vec::new(),
                    config: None,
                })],
                config: None,
            })
            .unwrap();

        let help = result.as_string().expect("Expected help text");
        assert!(help.contains("time.format"));
        assert!(help.contains("Usage:"));
        assert!(help.contains("Requires:\n  none"));
    }

    #[test]
    fn help_for_math_command_reports_plugin_docs() {
        let mut executor = executor();

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["help".into()],
                args: vec![Expr::Call(FunctionCall {
                    name: vec!["math".into(), "add".into()],
                    args: Vec::new(),
                    config: None,
                })],
                config: None,
            })
            .unwrap();

        let help = result.as_string().expect("Expected help text");
        assert!(help.contains("math.add"));
        assert!(help.contains("math.add <number> <number> [number...]"));
        assert!(help.contains("math.add 1 2 3"));
    }

    #[test]
    fn help_for_string_command_reports_plugin_docs() {
        let mut executor = executor();

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["help".into()],
                args: vec![Expr::Call(FunctionCall {
                    name: vec!["string".into(), "replace".into()],
                    args: Vec::new(),
                    config: None,
                })],
                config: None,
            })
            .unwrap();

        let help = result.as_string().expect("Expected help text");
        assert!(help.contains("string.replace"));
        assert!(help.contains("string.replace <text> <from> <to>"));
        assert!(help.contains("echo \"hello world\" | string.replace \"world\" \"zen\""));
    }

    #[test]
    fn help_for_second_batch_string_command_reports_plugin_docs() {
        let mut executor = executor();

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["help".into()],
                args: vec![Expr::Call(FunctionCall {
                    name: vec!["string".into(), "join".into()],
                    args: Vec::new(),
                    config: None,
                })],
                config: None,
            })
            .unwrap();

        let help = result.as_string().expect("Expected help text");
        assert!(help.contains("string.join"));
        assert!(help.contains("string.join <list> <delimiter>"));
        assert!(help.contains("string.split"));
    }

    #[test]
    fn help_for_workspace_command_reports_permission_and_docs() {
        let mut executor = executor();

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["help".into()],
                args: vec![Expr::Call(FunctionCall {
                    name: vec!["workspace".into(), "find".into()],
                    args: Vec::new(),
                    config: None,
                })],
                config: None,
            })
            .unwrap();

        let help = result.as_string().expect("Expected help text");
        assert!(help.contains("workspace.find"));
        assert!(help.contains("workspace.find <pattern>"));
        assert!(help.contains("workspace.read"));
    }

    #[test]
    fn help_for_state_command_reports_permission_and_docs() {
        let mut executor = executor();

        let result = executor
            .eval_call(FunctionCall {
                name: vec!["help".into()],
                args: vec![Expr::Call(FunctionCall {
                    name: vec!["state".into(), "save".into()],
                    args: Vec::new(),
                    config: None,
                })],
                config: None,
            })
            .unwrap();

        let help = result.as_string().expect("Expected help text");
        assert!(help.contains("state.save"));
        assert!(help.contains("Usage:"));
        assert!(help.contains("state.write"));
    }

    #[test]
    fn exec_runs_in_workdir() {
        let _guard = crate::interrupt::lock_for_test();
        crate::interrupt::clear_interrupt();
        let command = if cfg!(windows) { "cd" } else { "pwd" };

        let output = exec_command(ExecRequest {
            command: command.into(),
            argv: None,
            attempts: 1,
            timeout: None,
            wait_children: false,
            workdir: Some("src".into()),
            env: HashMap::new(),
            secret_values: Vec::new(),
        })
        .unwrap();

        let Value::Object(map) = output else {
            panic!("Expected exec output object");
        };

        match map.get("stdout") {
            Some(Value::String(value)) => assert!(
                value.trim_end().replace('\\', "/").ends_with("/src"),
                "Expected workdir stdout to end with /src, got {value:?}"
            ),
            other => panic!("Expected stdout string, got {:?}", other),
        }
        match map.get("cancelled") {
            Some(Value::Bool(value)) => assert!(!value),
            other => panic!("Expected cancelled bool, got {:?}", other),
        }
    }

    #[cfg(windows)]
    #[test]
    fn external_process_exec_passes_spaces_as_one_argument() {
        let _guard = crate::interrupt::lock_for_test();
        crate::interrupt::clear_interrupt();
        let mut executor = executor_with_exec_permission();
        let sql = "select id, body from notes where body = :body";
        let script_dir = env::current_dir()
            .unwrap()
            .join("target")
            .join("external-argv-tests");
        fs::create_dir_all(&script_dir).unwrap();
        let script = script_dir.join("print-first-arg.ps1");
        fs::write(&script, "[Console]::Out.WriteLine($args[0])\n").unwrap();

        let output = executor
            .external_process_exec(
                &format!(
                    r#"powershell -NoProfile -ExecutionPolicy Bypass -File "{}""#,
                    script.display()
                ),
                &FunctionCall {
                    name: vec!["sqlite".into(), "query".into()],
                    args: vec![Expr::string(sql)],
                    config: None,
                },
            )
            .unwrap();

        let Value::Object(map) = output else {
            panic!("Expected exec output object");
        };

        match map.get("stdout") {
            Some(Value::String(value)) => assert_eq!(value.trim_end(), sql),
            other => panic!("Expected stdout string, got {:?}", other),
        }
    }

    #[test]
    fn exec_reports_cancelled_when_interrupted() {
        let _guard = crate::interrupt::lock_for_test();
        crate::interrupt::request_interrupt();
        let command = if cfg!(windows) {
            "powershell -NoProfile -Command \"Start-Sleep -Seconds 5\""
        } else {
            "sleep 5"
        };

        let output = exec_command(ExecRequest {
            command: command.into(),
            argv: None,
            attempts: 1,
            timeout: None,
            wait_children: false,
            workdir: None,
            env: HashMap::new(),
            secret_values: Vec::new(),
        })
        .unwrap();
        crate::interrupt::clear_interrupt();

        let Value::Object(map) = output else {
            panic!("Expected exec output object");
        };

        match map.get("cancelled") {
            Some(Value::Bool(value)) => assert!(*value),
            other => panic!("Expected cancelled bool, got {:?}", other),
        }
        match map.get("timed_out") {
            Some(Value::Bool(value)) => assert!(!value),
            other => panic!("Expected timed_out bool, got {:?}", other),
        }
    }

    #[test]
    fn time_freeze_can_be_cleared() {
        let mut executor = executor();

        executor
            .time_value(
                vec![
                    Expr::Ident("freeze".into()),
                    Expr::String("2026-05-14T12:34:56Z".into()),
                ],
                Value::Null,
            )
            .unwrap();
        executor
            .time_value(
                vec![Expr::Ident("freeze".into()), Expr::String("clear".into())],
                Value::Null,
            )
            .unwrap();

        assert!(executor.mocked_time.is_none());
    }
}
