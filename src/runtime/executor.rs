use std::collections::HashMap;
use std::fs;

use crate::ast::*;
use crate::runtime::values::Value;
use crate::runtime::renderer;
use crate::permissions::PermissionSet;
use sysinfo::System;
use std::collections::HashSet;


pub struct Executor {
    env: HashMap<String, Value>,
    permissions: PermissionSet,
}

impl Executor {
    pub fn new_with_permissions(permissions: PermissionSet) -> Self {
        Self {
            env: HashMap::new(),
            permissions,
        }
    }

    pub fn execute(&mut self, program: Program) -> Result<(), String> {
        for stmt in program.statements {
            self.execute_stmt(stmt)?;
        }
        Ok(())
    }

    fn execute_stmt(&mut self, stmt: Stmt) -> Result<(), String> {
        match stmt {
            Stmt::Assignment { name, expr } => {
                let value = self.eval_expr(expr)?;
                self.env.insert(name, value);
            }

            Stmt::Pipeline(p) => {
                let result = self.eval_pipeline(p)?;
                renderer::render(&result);
            }

            Stmt::Expr(expr) => {
                let result = self.eval_expr(expr)?;
                renderer::render(&result);
            }
        }

        Ok(())
    }

    fn eval_expr(&mut self, expr: Expr) -> Result<Value, String> {
        match expr {
            Expr::Literal(l) => Ok(self.literal_to_value(l)),
            Expr::Ident(name) => self
                .env
                .get(&name)
                .cloned()
                .ok_or(format!("Undefined variable {}", name)),
            Expr::Call(call) => self.eval_call(call),
            Expr::Binary { left, op, right } => todo!(),
        }
    }

    fn literal_to_value(&self, lit: Literal) -> Value {
        match lit {
            Literal::String(s) => Value::String(s),
            Literal::Number(n) => Value::Number(n),
            Literal::Bool(b) => Value::Bool(b),
        }
    }

    fn eval_pipeline(&mut self, pipeline: Pipeline) -> Result<Value, String> {
        let mut value = self.eval_expr(pipeline.base)?;

        for stage in pipeline.stages {
            value = match stage {
                PipeStage::Where { expr } => {
                    self.pipe_where(value, expr)?
                }
                PipeStage::Select { fields } => {
                    self.pipe_select(value, fields)?
                }
                PipeStage::Sort { field, descending } => {
                    self.pipe_sort(value, field, descending)?
                }
                PipeStage::Limit { count } => {
                    self.pipe_limit(value, count)?
                }
                PipeStage::Count => {
                    self.pipe_count(value)?
                }
                PipeStage::Sum { field } => {
                    self.pipe_sum(value, field)?
                }
                PipeStage::Avg { field } => {
                    self.pipe_avg(value, field)?
                }
                PipeStage::Max { field } => {
                    self.pipe_max(value, field)?
                }
                PipeStage::Min { field } => {
                    self.pipe_min(value, field)?
                }
                PipeStage::Distinct { field } => {
                   self.pipe_distinct(value, field)?
                }
                PipeStage::Call(call) => {
                    self.eval_call_with_input(call, value)?
                }
            };
        }

        Ok(value)
    }

    fn pipe_where(&self, input: Value, expr: Expr) -> Result<Value, String> {
        if let Value::List(items) = input {
            let filtered = items
                .into_iter()
                .filter(|item| {
                    match self.eval_expr_on_item(expr.clone(), item) {
                        Value::Bool(b) => b,
                        _ => false,
                    }
                })
                .collect();

            Ok(Value::List(filtered))
        } else {
            Err("where can only operate on lists".into())
        }
    }

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

            Expr::Literal(l) => self.literal_to_value(l),

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

            // String equality
            (Value::String(a), Value::String(b), BinOp::Eq) => Value::Bool(a == b),
            (Value::String(a), Value::String(b), BinOp::Neq) => Value::Bool(a != b),

            _ => Value::Null,
        }
    }

    fn pipe_select(
        &self,
        input: Value,
        fields: Vec<String>,
    ) -> Result<Value, String> {
        if let Value::List(items) = input {
            let projected = items
                .into_iter()
                .map(|item| {
                    if let Value::Object(map) = item {
                        let mut new_map = HashMap::new();
                        for f in &fields {
                            if let Some(v) = map.get(f) {
                                new_map.insert(f.clone(), v.clone());
                            }
                        }
                        Value::Object(new_map)
                    } else {
                        item
                    }
                })
                .collect();

            Ok(Value::List(projected))
        } else {
            Err("select can only operate on lists".into())
        }
    }


    fn pipe_sort(
    &self,
    input: Value,
    field: String,
    descending: bool,
    ) -> Result<Value, String> {
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
                    (Some(Value::String(x)), Some(Value::String(y))) => {
                        x.cmp(y)
                    }
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


    fn pipe_limit(
        &self,
        input: Value,
        count: usize,
    ) -> Result<Value, String> {
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

    fn pipe_max(
        &self,
        input: Value,
        field: String,
    ) -> Result<Value, String> {
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

    fn pipe_min(
    &self,
    input: Value,
    field: String,
) -> Result<Value, String> {
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

    fn pipe_sum(
        &self,
        input: Value,
        field: String,
    ) -> Result<Value, String> {
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

    fn pipe_avg(
        &self,
        input: Value,
        field: String,
    ) -> Result<Value, String> {
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

fn pipe_distinct(
    &self,
    input: Value,
    field: String,
) -> Result<Value, String> {
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

    fn eval_call_with_input(
        &mut self,
        call: FunctionCall,
        input: Value,
    ) -> Result<Value, String> {
        let name = call.name.join(".");

        match name.as_str() {
            "fs.list" => {
                self.permissions.check("fs.read")?;
                let path = self.expect_string_arg(call.args)?;
                self.fs_list(path)
            }
            "process.list" => {
                self.permissions.check("proc.read")?;
                self.process_list()
            }

            "fs.copy" => {
                self.permissions.check("fs.write")?;
                let dest = self.expect_string_arg(call.args)?;
                self.fs_copy(input, dest)
            }


            _ => Err(format!("Unknown function {}", name)),
        }
    }

    fn expect_string_arg(&mut self, mut args: Vec<Expr>) -> Result<String, String> {
        if args.len() != 1 {
            return Err("Expected one string argument".into());
        }
        let mut args = args;
        let expr = args.pop().unwrap();

        let val = self.eval_expr(expr)?;

        val.as_string()
            .map(|s| s.to_string())
            .ok_or("Expected string argument".into())
    }

    fn fs_list(&self, path: String) -> Result<Value, String> {
        let entries = fs::read_dir(&path)
            .map_err(|e| format!("Failed to read dir: {}", e))?;

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

            map.insert(
                "pid".into(),
                Value::Number(pid.as_u32() as f64),
            );

            map.insert(
                "name".into(),
                Value::String(process.name().to_string()),
            );

            map.insert(
                "cpu".into(),
                Value::Number(process.cpu_usage() as f64),
            );

            map.insert(
                "memory".into(),
                Value::Number(process.memory() as f64),
            );

            list.push(Value::Object(map));
        }

        Ok(Value::List(list))
    }






    fn fs_copy(&self, input: Value, dest: String) -> Result<Value, String> {
        if let Value::List(items) = input {
            for item in items {
                if let Value::Object(map) = item {
                    if let Some(Value::String(name)) = map.get("name") {
                        let src_path = name;
                        let dest_path = format!("{}/{}", dest, name);
                        let _ = fs::copy(src_path, dest_path);
                    }
                }
            }

            Ok(Value::Null)
        } else {
            Err("fs.copy expects list input".into())
        }
    }
}