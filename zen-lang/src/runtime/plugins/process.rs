use crate::ast::FunctionCall;
#[cfg(test)]
use crate::runtime::executor::Executor;
use crate::runtime::plugin::{CommandDoc, PluginHost, PluginResult, ZenPlugin};
use crate::runtime::values::Value;

pub struct ProcessPlugin;

static PROCESS_DOCS: &[CommandDoc] = &[
    CommandDoc {
        command: "process.list",
        summary: "Lists running processes.",
        usage: "process.list",
        examples: &["process.list", "process.list | echo table"],
    },
    CommandDoc {
        command: "exec",
        summary: "Runs an external command and returns stdout, stderr, and exitcode.",
        usage:
            "exec <command> [retry N] [timeout DURATION] [workdir PATH] [{ env: { KEY: value } }]",
        examples: &[
            "exec pg_dump --version",
            "let result = exec pg_dump --version",
            "exec docker ps | parse json",
            "exec pg_dump retry 3 timeout 30s",
            "exec pg_dump { env: { PGPASSWORD: $pass } }",
        ],
    },
];

impl ZenPlugin for ProcessPlugin {
    fn name(&self) -> &'static str {
        "process"
    }

    fn commands(&self) -> &'static [&'static str] {
        &["process.list", "exec"]
    }

    fn command_permissions(&self) -> &'static [(&'static str, &'static str)] {
        &[("process.list", "proc.read"), ("exec", "proc.exec")]
    }

    fn command_docs(&self) -> &'static [CommandDoc] {
        PROCESS_DOCS
    }

    fn call(
        &self,
        executor: &mut dyn PluginHost,
        call: &FunctionCall,
        _input: &Value,
    ) -> Result<PluginResult, String> {
        match call.name.join(".").as_str() {
            "process.list" => executor.process_list_builtin().map(PluginResult::handled),
            "exec" => executor
                .process_exec(call.clone())
                .map(PluginResult::handled),
            _ => Ok(PluginResult::unhandled()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::FunctionCall;
    use crate::permissions::PermissionSet;

    #[test]
    fn process_plugin_ignores_unknown_calls() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = ProcessPlugin
            .call(
                &mut executor,
                &FunctionCall {
                    name: vec!["not_process".into()],
                    args: Vec::new(),
                    config: None,
                },
                &Value::Null,
            )
            .unwrap();

        assert!(!result.is_handled());
    }
}
