use crate::ast::FunctionCall;
use crate::runtime::executor::Executor;
use crate::runtime::plugin::{CommandDoc, PluginResult, ZenPlugin};
use crate::runtime::values::Value;

pub struct CorePlugin;

static CORE_DOCS: &[CommandDoc] = &[
    CommandDoc {
        command: "cd",
        summary: "Changes the Zen session working directory.",
        usage: "cd [path]",
        examples: &["cd src", "cd", "cd .."],
    },
    CommandDoc {
        command: "pwd",
        summary: "Prints the Zen session working directory.",
        usage: "pwd",
        examples: &["pwd"],
    },
    CommandDoc {
        command: "which",
        summary: "Resolves a builtin command or executable path.",
        usage: "which <command>",
        examples: &["which echo", "which fs.list", "which time.format"],
    },
    CommandDoc {
        command: "clear",
        summary: "Clears the terminal screen.",
        usage: "clear",
        examples: &["clear"],
    },
    CommandDoc {
        command: "echo",
        summary: "Prints values as text, JSON, table, success, or error output.",
        usage: "echo [-n] [json|table|success|error] [value...]",
        examples: &[
            "echo \"hello\"",
            "echo -n \"first\" \"second\"",
            "exec docker ps | parse json | echo table",
        ],
    },
    CommandDoc {
        command: "parse",
        summary: "Parses pipeline input into structured Zen values.",
        usage: "parse json",
        examples: &[
            "exec docker ps --format json | parse json",
            "echo result.stdout | parse json",
        ],
    },
    CommandDoc {
        command: "plugins.list",
        summary: "Lists loaded plugins, owned commands, and permission metadata.",
        usage: "plugins.list",
        examples: &[
            "plugins.list",
            "plugins.list | echo table",
            "plugins.list | echo json",
        ],
    },
    CommandDoc {
        command: "plugins.reload",
        summary: "Reloads builtin plugins and discovered external plugins.",
        usage: "plugins.reload",
        examples: &["plugins.reload", "plugins.reload | echo json"],
    },
    CommandDoc {
        command: "plugins.discover",
        summary: "Discovers external plugin manifests without loading or unloading anything.",
        usage: "plugins.discover",
        examples: &[
            "plugins.discover",
            "plugins.discover | fields name, status, version, source | echo table",
        ],
    },
    CommandDoc {
        command: "plugins.load",
        summary: "Loads an external plugin manifest or plugin directory from the workspace.",
        usage: "plugins.load <path>",
        examples: &[
            "plugins.load \".zen/plugins/hello\"",
            "plugins.load \".zen/plugins/hello/plugin.toml\"",
        ],
    },
    CommandDoc {
        command: "plugins.unload",
        summary: "Unloads an external plugin by name for the current session.",
        usage: "plugins.unload <name>",
        examples: &["plugins.unload hello"],
    },
    CommandDoc {
        command: "help",
        summary: "Shows builtin command help from plugin-owned docs.",
        usage: "help [command]",
        examples: &["help", "help exec", "help time.format"],
    },
];

impl ZenPlugin for CorePlugin {
    fn name(&self) -> &'static str {
        "core"
    }

    fn commands(&self) -> &'static [&'static str] {
        &[
            "cd",
            "pwd",
            "which",
            "clear",
            "echo",
            "parse",
            "plugins.list",
            "plugins.reload",
            "plugins.discover",
            "plugins.load",
            "plugins.unload",
            "help",
        ]
    }

    fn command_docs(&self) -> &'static [CommandDoc] {
        CORE_DOCS
    }

    fn call(
        &self,
        executor: &mut Executor,
        call: &FunctionCall,
        input: &Value,
    ) -> Result<PluginResult, String> {
        match call.name.join(".").as_str() {
            "cd" => executor
                .core_cd(call.args.clone())
                .map(PluginResult::handled),
            "pwd" => executor
                .core_pwd(call.args.clone())
                .map(PluginResult::handled),
            "which" => executor
                .core_which(call.args.clone())
                .map(PluginResult::handled),
            "clear" => executor
                .core_clear(call.args.clone())
                .map(PluginResult::handled),
            "echo" => executor
                .core_echo(call.args.clone(), input.clone())
                .map(PluginResult::handled),
            "parse" => executor
                .core_parse(call.args.clone(), input.clone())
                .map(PluginResult::handled),
            "plugins.list" => executor.plugins_list().map(PluginResult::handled),
            "plugins.reload" => executor
                .plugins_reload(call.args.clone())
                .map(PluginResult::handled),
            "plugins.discover" => executor
                .plugins_discover(call.args.clone())
                .map(PluginResult::handled),
            "plugins.load" => executor
                .plugins_load(call.args.clone())
                .map(PluginResult::handled),
            "plugins.unload" => executor
                .plugins_unload(call.args.clone())
                .map(PluginResult::handled),
            "help" => executor
                .core_help(call.args.clone())
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
    fn core_plugin_ignores_unknown_calls() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = CorePlugin
            .call(
                &mut executor,
                &FunctionCall {
                    name: vec!["not_core".into()],
                    args: Vec::new(),
                    config: None,
                },
                &Value::Null,
            )
            .unwrap();

        assert!(!result.is_handled());
    }
}
