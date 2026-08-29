use crate::ast::FunctionCall;
#[cfg(test)]
use crate::runtime::executor::Executor;
use crate::runtime::plugin::{CommandDoc, PluginHost, PluginResult, ZenPlugin};
use crate::runtime::values::Value;

pub struct WorkspacePlugin;

static WORKSPACE_DOCS: &[CommandDoc] = &[
    CommandDoc {
        command: "workspace.root",
        summary: "Returns the discovered workspace root.",
        usage: "workspace.root",
        examples: &["workspace.root"],
    },
    CommandDoc {
        command: "workspace.cwd",
        summary: "Returns the current Zen session working directory.",
        usage: "workspace.cwd",
        examples: &["workspace.cwd", "cd src\nworkspace.cwd"],
    },
    CommandDoc {
        command: "workspace.find",
        summary: "Finds files under the workspace root by a wildcard pattern.",
        usage: "workspace.find <pattern>",
        examples: &[
            "workspace.find \"*.rs\"",
            "workspace.find \"src/*.rs\" | echo table",
        ],
    },
    CommandDoc {
        command: "workspace.exists",
        summary: "Checks whether a path exists inside the workspace.",
        usage: "workspace.exists <path>",
        examples: &[
            "workspace.exists \"Cargo.toml\"",
            "workspace.exists \"migrations\"",
        ],
    },
    CommandDoc {
        command: "workspace.read",
        summary: "Reads a UTF-8 text file from the workspace.",
        usage: "workspace.read <path>",
        examples: &[
            "workspace.read \"Cargo.toml\"",
            "workspace.read \"schema.sql\"",
        ],
    },
    CommandDoc {
        command: "workspace.files",
        summary: "Lists files in a workspace directory.",
        usage: "workspace.files [path]",
        examples: &["workspace.files", "workspace.files \"src\" | echo table"],
    },
    CommandDoc {
        command: "workspace.dirs",
        summary: "Lists directories in a workspace directory.",
        usage: "workspace.dirs [path]",
        examples: &["workspace.dirs", "workspace.dirs \"src\" | echo table"],
    },
    CommandDoc {
        command: "workspace.env",
        summary: "Reads an environment variable.",
        usage: "workspace.env <name>",
        examples: &["workspace.env \"DATABASE_URL\""],
    },
];

impl ZenPlugin for WorkspacePlugin {
    fn name(&self) -> &'static str {
        "workspace"
    }

    fn commands(&self) -> &'static [&'static str] {
        &[
            "workspace.root",
            "workspace.cwd",
            "workspace.find",
            "workspace.exists",
            "workspace.read",
            "workspace.files",
            "workspace.dirs",
            "workspace.env",
        ]
    }

    fn command_permissions(&self) -> &'static [(&'static str, &'static str)] {
        &[
            ("workspace.find", "workspace.read"),
            ("workspace.exists", "workspace.read"),
            ("workspace.read", "workspace.read"),
            ("workspace.files", "workspace.read"),
            ("workspace.dirs", "workspace.read"),
            ("workspace.env", "workspace.env"),
        ]
    }

    fn command_docs(&self) -> &'static [CommandDoc] {
        WORKSPACE_DOCS
    }

    fn call(
        &self,
        executor: &mut dyn PluginHost,
        call: &FunctionCall,
        _input: &Value,
    ) -> Result<PluginResult, String> {
        let result = match call.name.join(".").as_str() {
            "workspace.root" => executor.workspace_root(call.args.clone()),
            "workspace.cwd" => executor.workspace_cwd(call.args.clone()),
            "workspace.find" => executor.workspace_find(call.args.clone()),
            "workspace.exists" => executor.workspace_exists(call.args.clone()),
            "workspace.read" => executor.workspace_read(call.args.clone()),
            "workspace.files" => executor.workspace_files(call.args.clone()),
            "workspace.dirs" => executor.workspace_dirs(call.args.clone()),
            "workspace.env" => executor.workspace_env(call.args.clone()),
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
    fn workspace_plugin_ignores_unknown_calls() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = WorkspacePlugin
            .call(
                &mut executor,
                &FunctionCall {
                    name: vec!["not_workspace".into()],
                    args: Vec::new(),
                    config: None,
                },
                &Value::Null,
            )
            .unwrap();

        assert!(!result.is_handled());
    }
}
