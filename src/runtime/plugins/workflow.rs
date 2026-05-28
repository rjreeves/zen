use crate::ast::FunctionCall;
use crate::runtime::executor::Executor;
use crate::runtime::plugin::{CommandDoc, PluginResult, ZenPlugin};
use crate::runtime::values::Value;

pub struct WorkflowPlugin;

static WORKFLOW_DOCS: &[CommandDoc] = &[CommandDoc {
    command: "workflow.run",
    summary: "Runs a small workflow object with step retry, failure actions, and finally actions.",
    usage: "workflow.run <workflow-object>",
    examples: &[
        "workflow.run { name: \"backup\", steps: [{ name: \"dump\", run: \"pg_dump mydb\", retry: { attempts: 3, delay: \"10s\" }, finally: [{ run: \"echo cleanup\" }] }] }",
        "workflow | workflow.run",
    ],
}];

impl ZenPlugin for WorkflowPlugin {
    fn name(&self) -> &'static str {
        "workflow"
    }

    fn commands(&self) -> &'static [&'static str] {
        &["workflow.run"]
    }

    fn command_permissions(&self) -> &'static [(&'static str, &'static str)] {
        &[("workflow.run", "proc.exec")]
    }

    fn command_docs(&self) -> &'static [CommandDoc] {
        WORKFLOW_DOCS
    }

    fn call(
        &self,
        executor: &mut Executor,
        call: &FunctionCall,
        input: &Value,
    ) -> Result<PluginResult, String> {
        match call.name.join(".").as_str() {
            "workflow.run" => executor
                .workflow_run(call.args.clone(), input.clone())
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
    fn workflow_plugin_ignores_unknown_calls() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = WorkflowPlugin
            .call(
                &mut executor,
                &FunctionCall {
                    name: vec!["not_workflow".into()],
                    args: Vec::new(),
                    config: None,
                },
                &Value::Null,
            )
            .unwrap();

        assert!(!result.is_handled());
    }
}
