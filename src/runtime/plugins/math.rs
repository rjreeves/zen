use crate::ast::{Expr, FunctionCall};
#[cfg(test)]
use crate::runtime::executor::Executor;
use crate::runtime::plugin::{CommandDoc, PluginHost, PluginResult, ZenPlugin};
use crate::runtime::values::Value;

pub struct MathPlugin;

static MATH_DOCS: &[CommandDoc] = &[
    CommandDoc {
        command: "math.add",
        summary: "Adds two or more numbers.",
        usage: "math.add <number> <number> [number...]",
        examples: &["math.add 1 2", "math.add 1 2 3"],
    },
    CommandDoc {
        command: "math.sub",
        summary: "Subtracts numbers from the first number.",
        usage: "math.sub <number> <number> [number...]",
        examples: &["math.sub 10 3", "math.sub 10 3 2"],
    },
    CommandDoc {
        command: "math.mul",
        summary: "Multiplies two or more numbers.",
        usage: "math.mul <number> <number> [number...]",
        examples: &["math.mul 3 4", "math.mul 2 3 4"],
    },
    CommandDoc {
        command: "math.div",
        summary: "Divides the first number by each following number.",
        usage: "math.div <number> <number> [number...]",
        examples: &["math.div 12 3", "math.div 100 2 5"],
    },
];

impl ZenPlugin for MathPlugin {
    fn name(&self) -> &'static str {
        "math"
    }

    fn commands(&self) -> &'static [&'static str] {
        &["math.add", "math.sub", "math.mul", "math.div"]
    }

    fn command_docs(&self) -> &'static [CommandDoc] {
        MATH_DOCS
    }

    fn call(
        &self,
        executor: &mut dyn PluginHost,
        call: &FunctionCall,
        _input: &Value,
    ) -> Result<PluginResult, String> {
        let result = match call.name.join(".").as_str() {
            "math.add" => self.fold_math(executor, &call.args, 0.0, |acc, value| acc + value),
            "math.sub" => self.reduce_math(executor, &call.args, |acc, value| acc - value),
            "math.mul" => self.fold_math(executor, &call.args, 1.0, |acc, value| acc * value),
            "math.div" => self.divide_math(executor, &call.args),
            _ => return Ok(PluginResult::unhandled()),
        }?;

        Ok(PluginResult::handled(Value::Number(result)))
    }
}

impl MathPlugin {
    fn fold_math(
        &self,
        executor: &mut dyn PluginHost,
        args: &[Expr],
        initial: f64,
        f: fn(f64, f64) -> f64,
    ) -> Result<f64, String> {
        if args.len() < 2 {
            return Err("math commands expect at least two numbers".into());
        }

        let mut result = initial;
        for arg in args {
            result = f(result, self.number_arg(executor, arg)?);
        }

        Ok(result)
    }

    fn reduce_math(
        &self,
        executor: &mut dyn PluginHost,
        args: &[Expr],
        f: fn(f64, f64) -> f64,
    ) -> Result<f64, String> {
        if args.len() < 2 {
            return Err("math commands expect at least two numbers".into());
        }

        let mut values = args.iter();
        let mut result = self.number_arg(executor, values.next().unwrap())?;

        for arg in values {
            let value = self.number_arg(executor, arg)?;
            result = f(result, value);
        }

        Ok(result)
    }

    fn divide_math(&self, executor: &mut dyn PluginHost, args: &[Expr]) -> Result<f64, String> {
        if args.len() < 2 {
            return Err("math commands expect at least two numbers".into());
        }

        let mut values = args.iter();
        let mut result = self.number_arg(executor, values.next().unwrap())?;

        for arg in values {
            let value = self.number_arg(executor, arg)?;
            if value == 0.0 {
                return Err("math.div cannot divide by zero".into());
            }
            result /= value;
        }

        Ok(result)
    }

    fn number_arg(&self, executor: &mut dyn PluginHost, arg: &Expr) -> Result<f64, String> {
        match executor.plugin_arg_value(arg.clone())? {
            Value::Number(value) => Ok(value),
            Value::String(value) => value
                .parse::<f64>()
                .map_err(|_| format!("Expected numeric argument, got '{}'", value)),
            other => Err(format!("Expected numeric argument, got {:?}", other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::FunctionCall;
    use crate::permissions::PermissionSet;

    #[test]
    fn math_plugin_ignores_unknown_calls() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = MathPlugin
            .call(
                &mut executor,
                &FunctionCall {
                    name: vec!["not_math".into()],
                    args: Vec::new(),
                    config: None,
                },
                &Value::Null,
            )
            .unwrap();

        assert!(!result.is_handled());
    }
}
