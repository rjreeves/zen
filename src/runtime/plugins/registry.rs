use crate::runtime::plugin::ZenPlugin;
use crate::runtime::plugins::archive::ArchivePlugin;
use crate::runtime::plugins::core::CorePlugin;
use crate::runtime::plugins::dropbox::DropboxPlugin;
use crate::runtime::plugins::external::discover_external_plugins;
use crate::runtime::plugins::fs::FsPlugin;
use crate::runtime::plugins::math::MathPlugin;
use crate::runtime::plugins::postgres::PostgresPlugin;
use crate::runtime::plugins::process::ProcessPlugin;
use crate::runtime::plugins::secrets::SecretsPlugin;
use crate::runtime::plugins::state::StatePlugin;
use crate::runtime::plugins::string::StringPlugin;
use crate::runtime::plugins::time::TimePlugin;
use crate::runtime::plugins::workflow::WorkflowPlugin;
use crate::runtime::plugins::workspace::WorkspacePlugin;
use std::sync::Arc;

pub fn builtin_plugins() -> Vec<Arc<dyn ZenPlugin>> {
    let mut plugins: Vec<Arc<dyn ZenPlugin>> = vec![
        Arc::new(ArchivePlugin),
        Arc::new(CorePlugin),
        Arc::new(DropboxPlugin),
        Arc::new(FsPlugin),
        Arc::new(MathPlugin),
        Arc::new(PostgresPlugin),
        Arc::new(ProcessPlugin),
        Arc::new(SecretsPlugin),
        Arc::new(StatePlugin),
        Arc::new(StringPlugin),
        Arc::new(TimePlugin),
        Arc::new(WorkflowPlugin),
        Arc::new(WorkspacePlugin),
    ];

    plugins.extend(discover_external_plugins());
    plugins
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_plugin_order_is_stable() {
        let plugins = builtin_plugins();
        let names: Vec<_> = plugins.iter().map(|plugin| plugin.name()).collect();

        assert_eq!(
            &names[..13],
            vec![
                "archive",
                "core",
                "dropbox",
                "fs",
                "math",
                "postgres",
                "process",
                "secrets",
                "state",
                "string",
                "time",
                "workflow",
                "workspace"
            ]
        );
    }

    #[test]
    fn builtin_plugins_expose_expected_commands() {
        let plugins = builtin_plugins();
        let commands: Vec<_> = plugins
            .iter()
            .flat_map(|plugin| plugin.commands().iter().copied())
            .collect();

        assert!(commands.contains(&"help"));
        assert!(commands.contains(&"archive.zip"));
        assert!(commands.contains(&"archive.list"));
        assert!(commands.contains(&"dropbox.status"));
        assert!(commands.contains(&"dropbox.account"));
        assert!(commands.contains(&"dropbox.list"));
        assert!(commands.contains(&"dropbox.secrets.import"));
        assert!(commands.contains(&"fs.list"));
        assert!(commands.contains(&"math.add"));
        assert!(commands.contains(&"pg.query"));
        assert!(commands.contains(&"pg.auth.passwordless"));
        assert!(commands.contains(&"exec"));
        assert!(commands.contains(&"secrets.get"));
        assert!(commands.contains(&"state.save"));
        assert!(commands.contains(&"state.clear"));
        assert!(commands.contains(&"string.upper"));
        assert!(commands.contains(&"string.split"));
        assert!(commands.contains(&"time.format"));
        assert!(commands.contains(&"workflow.run"));
        assert!(commands.contains(&"workspace.root"));
        assert!(commands.contains(&"workspace.read"));
    }
}
