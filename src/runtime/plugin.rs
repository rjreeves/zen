use crate::ast::FunctionCall;
use crate::runtime::executor::Executor;
use crate::runtime::values::Value;

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
        executor: &mut Executor,
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
            _executor: &mut Executor,
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
