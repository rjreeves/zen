use crate::ast::FunctionCall;
#[cfg(test)]
use crate::runtime::executor::Executor;
use crate::runtime::plugin::{CommandDoc, PluginHost, PluginResult, ZenPlugin};
use crate::runtime::values::Value;

pub struct FsPlugin;

static FS_DOCS: &[CommandDoc] = &[
    CommandDoc {
        command: "fs.list",
        summary: "Lists entries in a directory.",
        usage: "fs.list <path>",
        examples: &["fs.list \".\"", "fs.list \"src\" | echo table"],
    },
    CommandDoc {
        command: "fs.copy",
        summary: "Copies one file, or file entries from pipeline input, inside the workspace.",
        usage: "fs.copy <source> <destination>\nfs.copy <destination>",
        examples: &[
            "fs.copy \"target/release/zen.exe\" \"dist/zen.exe\"",
            "fs.list \"src\" | fs.copy \"backup\"",
        ],
    },
];

impl ZenPlugin for FsPlugin {
    fn name(&self) -> &'static str {
        "fs"
    }

    fn commands(&self) -> &'static [&'static str] {
        &["fs.list", "fs.copy"]
    }

    fn command_permissions(&self) -> &'static [(&'static str, &'static str)] {
        &[
            ("fs.list", "fs.read"),
            ("fs.copy", "fs.read"),
            ("fs.copy", "fs.write"),
        ]
    }

    fn command_docs(&self) -> &'static [CommandDoc] {
        FS_DOCS
    }

    fn call(
        &self,
        executor: &mut dyn PluginHost,
        call: &FunctionCall,
        input: &Value,
    ) -> Result<PluginResult, String> {
        let result = match call.name.join(".").as_str() {
            "fs.list" => executor.fs_list_builtin(call.args.clone()),
            "fs.copy" => executor.fs_copy_builtin(call.args.clone(), input.clone()),
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
    fn fs_plugin_ignores_unknown_calls() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = FsPlugin
            .call(
                &mut executor,
                &FunctionCall {
                    name: vec!["not_fs".into()],
                    args: Vec::new(),
                    config: None,
                },
                &Value::Null,
            )
            .unwrap();

        assert!(!result.is_handled());
    }
}
