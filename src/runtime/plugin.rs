use crate::ast::{Expr, FunctionCall};
use crate::runtime::values::Value;
use std::path::PathBuf;

/// The surface a `ZenPlugin` is allowed to touch on the runtime host. This
/// exists so plugins depend on "something that can check permissions, read
/// the workspace, etc.", not on the concrete `Executor` (which also carries
/// the .fg interpreter's eval state, session variables, and plugin dispatch).
/// `Executor` implements this by delegating to its own existing methods.
pub trait PluginHost {
    fn check_permission(&self, permission: &str) -> Result<(), String>;
    fn plugin_arg_value(&mut self, expr: Expr) -> Result<Value, String>;
    fn resolve_workspace_path(&self, path: &str) -> Result<PathBuf, String>;
    fn resolve_local_write_path(&self, path: &str) -> Result<PathBuf, String>;

    fn core_echo(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String>;
    fn core_parse(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String>;
    fn core_which(&mut self, args: Vec<Expr>) -> Result<Value, String>;
    fn core_clear(&mut self, args: Vec<Expr>) -> Result<Value, String>;
    fn core_cd(&mut self, args: Vec<Expr>) -> Result<Value, String>;
    fn core_pwd(&mut self, args: Vec<Expr>) -> Result<Value, String>;
    fn core_help(&mut self, args: Vec<Expr>) -> Result<Value, String>;

    fn plugins_reload(&mut self, args: Vec<Expr>) -> Result<Value, String>;
    fn plugins_discover(&mut self, args: Vec<Expr>) -> Result<Value, String>;
    fn plugins_load(&mut self, args: Vec<Expr>) -> Result<Value, String>;
    fn plugins_unload(&mut self, args: Vec<Expr>) -> Result<Value, String>;

    fn process_exec(&mut self, call: FunctionCall) -> Result<Value, String>;
    fn external_process_exec(
        &mut self,
        base_command: &str,
        call: &FunctionCall,
    ) -> Result<Value, String>;

    fn workflow_run(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String>;

    fn workspace_root(&mut self, args: Vec<Expr>) -> Result<Value, String>;
    fn workspace_cwd(&mut self, args: Vec<Expr>) -> Result<Value, String>;
    fn workspace_find(&mut self, args: Vec<Expr>) -> Result<Value, String>;
    fn workspace_exists(&mut self, args: Vec<Expr>) -> Result<Value, String>;
    fn workspace_read(&mut self, args: Vec<Expr>) -> Result<Value, String>;
    fn workspace_files(&mut self, args: Vec<Expr>) -> Result<Value, String>;
    fn workspace_dirs(&mut self, args: Vec<Expr>) -> Result<Value, String>;
    fn workspace_env(&mut self, args: Vec<Expr>) -> Result<Value, String>;

    fn state_save(&mut self, args: Vec<Expr>) -> Result<Value, String>;
    fn state_load(&mut self, args: Vec<Expr>) -> Result<Value, String>;
    fn state_clear(&mut self, args: Vec<Expr>) -> Result<Value, String>;
    fn state_list(&mut self, args: Vec<Expr>) -> Result<Value, String>;

    fn time_builtin(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String>;
    fn time_builtin_with_mode(
        &mut self,
        mode: &str,
        args: Vec<Expr>,
        input: Value,
    ) -> Result<Value, String>;
    fn time_local(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String>;
    fn time_local_with_mode(
        &mut self,
        mode: &str,
        args: Vec<Expr>,
        input: Value,
    ) -> Result<Value, String>;
    fn time_freeze(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String>;
    fn time_measure(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String>;
    fn time_benchmark(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String>;
    fn time_sleep(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String>;

    fn fs_list_builtin(&mut self, args: Vec<Expr>) -> Result<Value, String>;
    fn fs_copy_builtin(&mut self, args: Vec<Expr>, input: Value) -> Result<Value, String>;
    fn process_list_builtin(&mut self) -> Result<Value, String>;
    fn plugins_list(&self) -> Result<Value, String>;
}

pub struct CommandDoc {
    pub command: &'static str,
    pub summary: &'static str,
    pub usage: &'static str,
    pub examples: &'static [&'static str],
}

pub enum PluginResult {
    Handled(Value),
    Unhandled,
}

impl PluginResult {
    pub fn handled(value: Value) -> Self {
        Self::Handled(value)
    }

    pub fn unhandled() -> Self {
        Self::Unhandled
    }

    #[allow(dead_code)]
    pub fn is_handled(&self) -> bool {
        matches!(self, Self::Handled(_))
    }
}

pub trait ZenPlugin {
    fn name(&self) -> &'static str;

    fn description(&self) -> Option<&'static str> {
        None
    }

    fn version(&self) -> Option<&'static str> {
        None
    }

    fn author(&self) -> Option<&'static str> {
        None
    }

    fn homepage(&self) -> Option<&'static str> {
        None
    }

    fn kind(&self) -> &'static str {
        "builtin"
    }

    fn source(&self) -> Option<&'static str> {
        None
    }

    fn commands(&self) -> &'static [&'static str] {
        &[]
    }

    fn command_permissions(&self) -> &'static [(&'static str, &'static str)] {
        &[]
    }

    fn command_docs(&self) -> &'static [CommandDoc] {
        &[]
    }

    fn call(
        &self,
        executor: &mut dyn PluginHost,
        call: &FunctionCall,
        input: &Value,
    ) -> Result<PluginResult, String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_result_reports_handled_state() {
        assert!(PluginResult::handled(Value::Null).is_handled());
        assert!(!PluginResult::unhandled().is_handled());
    }

    struct EmptyPlugin;

    impl ZenPlugin for EmptyPlugin {
        fn name(&self) -> &'static str {
            "empty"
        }

        fn call(
            &self,
            _executor: &mut dyn PluginHost,
            _call: &FunctionCall,
            _input: &Value,
        ) -> Result<PluginResult, String> {
            Ok(PluginResult::unhandled())
        }
    }

    #[test]
    fn plugin_commands_default_to_empty() {
        assert!(EmptyPlugin.commands().is_empty());
    }

    #[test]
    fn plugin_kind_defaults_to_builtin() {
        assert_eq!(EmptyPlugin.kind(), "builtin");
    }

    #[test]
    fn plugin_metadata_defaults_to_none() {
        assert_eq!(EmptyPlugin.description(), None);
        assert_eq!(EmptyPlugin.version(), None);
        assert_eq!(EmptyPlugin.author(), None);
        assert_eq!(EmptyPlugin.homepage(), None);
    }

    #[test]
    fn plugin_source_defaults_to_none() {
        assert_eq!(EmptyPlugin.source(), None);
    }

    #[test]
    fn plugin_command_permissions_default_to_empty() {
        assert!(EmptyPlugin.command_permissions().is_empty());
    }

    #[test]
    fn plugin_command_docs_default_to_empty() {
        assert!(EmptyPlugin.command_docs().is_empty());
    }
}
