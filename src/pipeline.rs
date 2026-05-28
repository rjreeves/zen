use crate::ast::{Expr, PipelineStage};
use crate::executor::{eval_expr, Value, Context};

pub fn execute_pipeline(
    input: Value,
    stages: &[PipelineStage],
    ctx: &mut Context,
) -> Result<Value, String> {
    let mut current = input;

    for stage in stages {
        current = match stage {
            PipelineStage::Where(expr) => apply_where(current, expr, ctx)?,
            PipelineStage::Select(fields) => apply_select(current, fields)?,
        };
    }

    Ok(current)
}
    fn apply_where(
    input: Value,
    expr: &Expr,
    ctx: &mut Context,
) -> Result<Value, String> {
    let list = match input {
        Value::List(v) => v,
        _ => return Err("where expects a list".into()),
    };

    let mut out = Vec::new();

    for item in list {
        let mut local_ctx = ctx.clone();

        if let Value::Object(map) = &item {
            for (k, v) in map {
                local_ctx.vars.insert(k.clone(), v.clone());
            }
        }

        let result = eval_expr(expr, &mut local_ctx)?;

        if matches!(result, Value::Bool(true)) {
            out.push(item);
        }
    }

    Ok(Value::List(out))
}
    fn apply_select(
    input: Value,
    fields: &[String],
) -> Result<Value, String> {
    let list = match input {
        Value::List(v) => v,
        _ => return Err("select expects a list".into()),
    };

    let mut out = Vec::new();

    for item in list {
        match item {
            Value::Object(map) => {
                let mut new_map = std::collections::HashMap::new();

                for field in fields {
                    if let Some(v) = map.get(field) {
                        new_map.insert(field.clone(), v.clone());
                    }
                }

                out.push(Value::Object(new_map));
            }

            _ => return Err("select expects objects".into()),
        }
    }

    Ok(Value::List(out))
}