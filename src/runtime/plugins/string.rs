use crate::ast::{Expr, FunctionCall};
#[cfg(test)]
use crate::runtime::executor::Executor;
use crate::runtime::plugin::{CommandDoc, PluginHost, PluginResult, ZenPlugin};
use crate::runtime::values::Value;

pub struct StringPlugin;

static STRING_DOCS: &[CommandDoc] = &[
    CommandDoc {
        command: "string.upper",
        summary: "Converts text to uppercase.",
        usage: "string.upper [text]",
        examples: &["string.upper \"hello\"", "echo \"hello\" | string.upper"],
    },
    CommandDoc {
        command: "string.lower",
        summary: "Converts text to lowercase.",
        usage: "string.lower [text]",
        examples: &["string.lower \"HELLO\"", "echo \"HELLO\" | string.lower"],
    },
    CommandDoc {
        command: "string.trim",
        summary: "Removes leading and trailing whitespace.",
        usage: "string.trim [text]",
        examples: &[
            "string.trim \"  hello  \"",
            "echo \"  hello  \" | string.trim",
        ],
    },
    CommandDoc {
        command: "string.len",
        summary: "Returns the character length of text.",
        usage: "string.len [text]",
        examples: &["string.len \"hello\"", "echo \"hello\" | string.len"],
    },
    CommandDoc {
        command: "string.contains",
        summary: "Checks whether text contains a substring.",
        usage: "string.contains <text> <needle>",
        examples: &[
            "string.contains \"hello world\" \"world\"",
            "echo \"hello world\" | string.contains \"world\"",
        ],
    },
    CommandDoc {
        command: "string.replace",
        summary: "Replaces matching text with new text.",
        usage: "string.replace <text> <from> <to>",
        examples: &[
            "string.replace \"hello world\" \"world\" \"zen\"",
            "echo \"hello world\" | string.replace \"world\" \"zen\"",
        ],
    },
    CommandDoc {
        command: "string.split",
        summary: "Splits text into a list of strings.",
        usage: "string.split <text> <delimiter>",
        examples: &[
            "string.split \"a,b,c\" \",\"",
            "echo \"a,b,c\" | string.split \",\"",
        ],
    },
    CommandDoc {
        command: "string.join",
        summary: "Joins a list of values into text.",
        usage: "string.join <list> <delimiter>",
        examples: &[
            "let parts = string.split \"a,b,c\" \",\"",
            "string.join parts \" | \"",
            "echo \"a,b,c\" | string.split \",\" | string.join \" | \"",
        ],
    },
    CommandDoc {
        command: "string.starts_with",
        summary: "Checks whether text starts with a prefix.",
        usage: "string.starts_with <text> <prefix>",
        examples: &[
            "string.starts_with \"hello\" \"he\"",
            "echo \"hello\" | string.starts_with \"he\"",
        ],
    },
    CommandDoc {
        command: "string.ends_with",
        summary: "Checks whether text ends with a suffix.",
        usage: "string.ends_with <text> <suffix>",
        examples: &[
            "string.ends_with \"hello\" \"lo\"",
            "echo \"hello\" | string.ends_with \"lo\"",
        ],
    },
];

impl ZenPlugin for StringPlugin {
    fn name(&self) -> &'static str {
        "string"
    }

    fn commands(&self) -> &'static [&'static str] {
        &[
            "string.upper",
            "string.lower",
            "string.trim",
            "string.len",
            "string.contains",
            "string.replace",
            "string.split",
            "string.join",
            "string.starts_with",
            "string.ends_with",
        ]
    }

    fn command_docs(&self) -> &'static [CommandDoc] {
        STRING_DOCS
    }

    fn call(
        &self,
        executor: &mut dyn PluginHost,
        call: &FunctionCall,
        input: &Value,
    ) -> Result<PluginResult, String> {
        let result = match call.name.join(".").as_str() {
            "string.upper" => Value::String(
                self.target_text(executor, &call.args, input)?
                    .to_uppercase(),
            ),
            "string.lower" => Value::String(
                self.target_text(executor, &call.args, input)?
                    .to_lowercase(),
            ),
            "string.trim" => Value::String(
                self.target_text(executor, &call.args, input)?
                    .trim()
                    .to_string(),
            ),
            "string.len" => Value::Number(
                self.target_text(executor, &call.args, input)?
                    .chars()
                    .count() as f64,
            ),
            "string.contains" => {
                let (text, needle) = self.binary_text_args(
                    executor,
                    &call.args,
                    input,
                    "string.contains",
                    "needle",
                )?;
                Value::Bool(text.contains(&needle))
            }
            "string.replace" => {
                let (text, from, to) = self.ternary_text_args(executor, &call.args, input)?;
                Value::String(text.replace(&from, &to))
            }
            "string.split" => {
                let (text, delimiter) = self.binary_text_args(
                    executor,
                    &call.args,
                    input,
                    "string.split",
                    "delimiter",
                )?;
                Value::List(
                    text.split(&delimiter)
                        .map(|part| Value::String(part.to_string()))
                        .collect(),
                )
            }
            "string.join" => {
                let (items, delimiter) = self.join_args(executor, &call.args, input)?;
                Value::String(
                    items
                        .into_iter()
                        .map(|item| self.value_to_text(item))
                        .collect::<Result<Vec<_>, _>>()?
                        .join(&delimiter),
                )
            }
            "string.starts_with" => {
                let (text, prefix) = self.binary_text_args(
                    executor,
                    &call.args,
                    input,
                    "string.starts_with",
                    "prefix",
                )?;
                Value::Bool(text.starts_with(&prefix))
            }
            "string.ends_with" => {
                let (text, suffix) = self.binary_text_args(
                    executor,
                    &call.args,
                    input,
                    "string.ends_with",
                    "suffix",
                )?;
                Value::Bool(text.ends_with(&suffix))
            }
            _ => return Ok(PluginResult::unhandled()),
        };

        Ok(PluginResult::handled(result))
    }
}

impl StringPlugin {
    fn target_text(
        &self,
        executor: &mut dyn PluginHost,
        args: &[Expr],
        input: &Value,
    ) -> Result<String, String> {
        if let Some(arg) = args.first() {
            return self.text_arg(executor, arg);
        }

        self.input_text(input)
    }

    fn binary_text_args(
        &self,
        executor: &mut dyn PluginHost,
        args: &[Expr],
        input: &Value,
        command: &str,
        second_name: &str,
    ) -> Result<(String, String), String> {
        if matches!(input, Value::Null) {
            if args.len() != 2 {
                return Err(format!("{} expects text and {}", command, second_name));
            }

            return Ok((
                self.text_arg(executor, &args[0])?,
                self.text_arg(executor, &args[1])?,
            ));
        }

        if args.len() != 1 {
            return Err(format!(
                "{} expects {} when used in a pipeline",
                command, second_name
            ));
        }

        Ok((self.input_text(input)?, self.text_arg(executor, &args[0])?))
    }

    fn ternary_text_args(
        &self,
        executor: &mut dyn PluginHost,
        args: &[Expr],
        input: &Value,
    ) -> Result<(String, String, String), String> {
        if matches!(input, Value::Null) {
            if args.len() != 3 {
                return Err("string.replace expects text, from, and to".into());
            }

            return Ok((
                self.text_arg(executor, &args[0])?,
                self.text_arg(executor, &args[1])?,
                self.text_arg(executor, &args[2])?,
            ));
        }

        if args.len() != 2 {
            return Err("string.replace expects from and to when used in a pipeline".into());
        }

        Ok((
            self.input_text(input)?,
            self.text_arg(executor, &args[0])?,
            self.text_arg(executor, &args[1])?,
        ))
    }

    fn join_args(
        &self,
        executor: &mut dyn PluginHost,
        args: &[Expr],
        input: &Value,
    ) -> Result<(Vec<Value>, String), String> {
        if matches!(input, Value::Null) {
            if args.len() != 2 {
                return Err("string.join expects list and delimiter".into());
            }

            return Ok((
                self.list_arg(executor, &args[0])?,
                self.text_arg(executor, &args[1])?,
            ));
        }

        if args.len() != 1 {
            return Err("string.join expects delimiter when used in a pipeline".into());
        }

        Ok((self.input_list(input)?, self.text_arg(executor, &args[0])?))
    }

    fn text_arg(&self, executor: &mut dyn PluginHost, arg: &Expr) -> Result<String, String> {
        self.value_to_text(executor.plugin_arg_value(arg.clone())?)
    }

    fn list_arg(&self, executor: &mut dyn PluginHost, arg: &Expr) -> Result<Vec<Value>, String> {
        self.value_to_list(executor.plugin_arg_value(arg.clone())?)
    }

    fn input_text(&self, input: &Value) -> Result<String, String> {
        self.value_to_text(input.clone())
    }

    fn input_list(&self, input: &Value) -> Result<Vec<Value>, String> {
        self.value_to_list(input.clone())
    }

    fn value_to_text(&self, value: Value) -> Result<String, String> {
        match value {
            Value::String(value) => Ok(value),
            Value::Number(value) => Ok(value.to_string()),
            Value::Bool(value) => Ok(value.to_string()),
            Value::Null => Err("string commands expect text input".into()),
            other => Err(format!(
                "string commands expect text input, got {:?}",
                other
            )),
        }
    }

    fn value_to_list(&self, value: Value) -> Result<Vec<Value>, String> {
        match value {
            Value::List(items) => Ok(items),
            other => Err(format!("string.join expects list input, got {:?}", other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::FunctionCall;
    use crate::permissions::PermissionSet;

    #[test]
    fn string_plugin_ignores_unknown_calls() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = StringPlugin
            .call(
                &mut executor,
                &FunctionCall {
                    name: vec!["not_string".into()],
                    args: Vec::new(),
                    config: None,
                },
                &Value::Null,
            )
            .unwrap();

        assert!(!result.is_handled());
    }
}
