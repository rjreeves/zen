use crate::ast::FunctionCall;
#[cfg(test)]
use crate::runtime::executor::Executor;
use crate::runtime::plugin::{CommandDoc, PluginHost, PluginResult, ZenPlugin};
use crate::runtime::values::Value;

pub struct StatePlugin;

static STATE_DOCS: &[CommandDoc] = &[
    CommandDoc {
        command: "state.save",
        summary: "Saves current session variables to the workspace state file.",
        usage: "state.save",
        examples: &["state.save"],
    },
    CommandDoc {
        command: "state.load",
        summary: "Loads session variables from the workspace state file.",
        usage: "state.load",
        examples: &["state.load"],
    },
    CommandDoc {
        command: "state.clear",
        summary: "Deletes the saved workspace state file without changing live variables.",
        usage: "state.clear",
        examples: &["state.clear"],
    },
    CommandDoc {
        command: "state.list",
        summary: "Lists current session variables.",
        usage: "state.list",
        examples: &["state.list", "state.list | echo table"],
    },
];

impl ZenPlugin for StatePlugin {
    fn name(&self) -> &'static str {
        "state"
    }

    fn commands(&self) -> &'static [&'static str] {
        &["state.save", "state.load", "state.clear", "state.list"]
    }

    fn command_permissions(&self) -> &'static [(&'static str, &'static str)] {
        &[
            ("state.save", "state.write"),
            ("state.load", "state.read"),
            ("state.clear", "state.write"),
        ]
    }

    fn command_docs(&self) -> &'static [CommandDoc] {
        STATE_DOCS
    }

    fn call(
        &self,
        executor: &mut dyn PluginHost,
        call: &FunctionCall,
        _input: &Value,
    ) -> Result<PluginResult, String> {
        let result = match call.name.join(".").as_str() {
            "state.save" => executor.state_save(call.args.clone()),
            "state.load" => executor.state_load(call.args.clone()),
            "state.clear" => executor.state_clear(call.args.clone()),
            "state.list" => executor.state_list(call.args.clone()),
            _ => return Ok(PluginResult::unhandled()),
        }?;

        Ok(PluginResult::handled(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::FunctionCall;
    use crate::permissions::PermissionSet;

    #[test]
    fn state_plugin_ignores_unknown_calls() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = StatePlugin
            .call(
                &mut executor,
                &FunctionCall {
                    name: vec!["not_state".into()],
                    args: Vec::new(),
                    config: None,
                },
                &Value::Null,
            )
            .unwrap();

        assert!(!result.is_handled());
    }
}
