use crate::ast::FunctionCall;
#[cfg(test)]
use crate::runtime::executor::Executor;
use crate::runtime::plugin::{CommandDoc, PluginHost, PluginResult, ZenPlugin};
use crate::runtime::values::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Clone, Default)]
pub struct ExternalPluginDiagnostics {
    pub root: Option<String>,
    pub loaded: Vec<ExternalPluginLoaded>,
    pub failed: Vec<ExternalPluginFailed>,
}

#[derive(Clone)]
pub struct ExternalPluginLoaded {
    pub name: String,
    pub source: String,
    pub commands: Vec<String>,
}

#[derive(Clone)]
pub struct ExternalPluginFailed {
    pub source: String,
    pub error: String,
}

pub struct ExternalPluginDiscovery {
    pub name: Option<String>,
    pub description: Option<String>,
    pub version: Option<String>,
    pub author: Option<String>,
    pub homepage: Option<String>,
    pub source: String,
    pub commands: Vec<String>,
    pub error: Option<String>,
}

static DIAGNOSTICS: OnceLock<Mutex<ExternalPluginDiagnostics>> = OnceLock::new();

pub fn discover_external_plugins() -> Vec<Arc<dyn ZenPlugin>> {
    let Some(plugins_dir) = external_plugin_search_roots()
        .into_iter()
        .find(|plugins_dir| plugins_dir.is_dir())
    else {
        record_external_diagnostics(ExternalPluginDiagnostics::default());
        return Vec::new();
    };

    load_external_plugins_from(&plugins_dir)
}

fn external_plugin_search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Ok(cwd) = std::env::current_dir() {
        roots.extend(cwd.ancestors().map(|dir| dir.join(".zen").join("plugins")));
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            roots.push(exe_dir.join(".zen").join("plugins"));
        }
    }

    if let Some(base) = dirs::config_dir()
        .or_else(dirs::data_local_dir)
        .or_else(dirs::data_dir)
        .or_else(dirs::home_dir)
    {
        roots.push(base.join("zen").join(".zen").join("plugins"));
        roots.push(base.join("zen").join("plugins"));
    }

    dedupe_paths(roots)
}

fn dedupe_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut deduped = Vec::new();
    for path in paths {
        if !deduped.iter().any(|existing| existing == &path) {
            deduped.push(path);
        }
    }
    deduped
}

pub fn load_external_plugins_from(root: &Path) -> Vec<Arc<dyn ZenPlugin>> {
    let Ok(entries) = fs::read_dir(root) else {
        record_external_diagnostics(ExternalPluginDiagnostics {
            root: Some(root.display().to_string()),
            loaded: Vec::new(),
            failed: vec![ExternalPluginFailed {
                source: root.display().to_string(),
                error: "could not read plugin directory".into(),
            }],
        });
        return Vec::new();
    };

    let mut plugins = Vec::new();
    let mut diagnostics = ExternalPluginDiagnostics {
        root: Some(root.display().to_string()),
        loaded: Vec::new(),
        failed: Vec::new(),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let manifest_path = path.join("plugin.toml");
        if !manifest_path.is_file() {
            continue;
        }

        match load_external_plugin(&manifest_path) {
            Ok((plugin, loaded)) => {
                diagnostics.loaded.push(loaded);
                plugins.push(plugin);
            }
            Err(error) => {
                diagnostics.failed.push(ExternalPluginFailed {
                    source: manifest_path.display().to_string(),
                    error: error.clone(),
                });
                eprintln!(
                    "Could not load external plugin {}: {}",
                    manifest_path.display(),
                    error
                );
            }
        }
    }

    record_external_diagnostics(diagnostics);
    plugins
}

pub fn external_plugin_diagnostics() -> ExternalPluginDiagnostics {
    diagnostics_store()
        .lock()
        .map(|diagnostics| diagnostics.clone())
        .unwrap_or_default()
}

pub fn discover_external_plugin_manifests(root: &Path) -> Vec<ExternalPluginDiscovery> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };

    let mut discovered = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let manifest_path = path.join("plugin.toml");
        if !manifest_path.is_file() {
            continue;
        }

        let source = manifest_path.display().to_string();
        match fs::read_to_string(&manifest_path)
            .map_err(|error| format!("could not read manifest: {}", error))
            .and_then(|text| parse_manifest(&text))
        {
            Ok(manifest) => discovered.push(ExternalPluginDiscovery {
                name: Some(manifest.name),
                description: manifest.description,
                version: manifest.version,
                author: manifest.author,
                homepage: manifest.homepage,
                source,
                commands: manifest
                    .commands
                    .into_iter()
                    .map(|command| command.name)
                    .collect(),
                error: None,
            }),
            Err(error) => discovered.push(ExternalPluginDiscovery {
                name: None,
                description: None,
                version: None,
                author: None,
                homepage: None,
                source,
                commands: Vec::new(),
                error: Some(error),
            }),
        }
    }

    discovered
}

pub fn load_external_plugin_from_path(
    path: &Path,
) -> Result<(Arc<dyn ZenPlugin>, ExternalPluginLoaded), String> {
    let manifest_path = if path.is_dir() {
        path.join("plugin.toml")
    } else {
        path.to_path_buf()
    };

    load_external_plugin(&manifest_path)
}

pub fn record_external_plugin_loaded(loaded: ExternalPluginLoaded) {
    if let Ok(mut diagnostics) = diagnostics_store().lock() {
        diagnostics
            .loaded
            .retain(|existing| existing.name != loaded.name);
        diagnostics.loaded.push(loaded);
    }
}

pub fn record_external_plugin_unloaded(name: &str) {
    if let Ok(mut diagnostics) = diagnostics_store().lock() {
        diagnostics.loaded.retain(|existing| existing.name != name);
    }
}

fn load_external_plugin(path: &Path) -> Result<(Arc<dyn ZenPlugin>, ExternalPluginLoaded), String> {
    let text =
        fs::read_to_string(path).map_err(|error| format!("could not read manifest: {}", error))?;
    let manifest = parse_manifest(&text)?;
    let loaded = ExternalPluginLoaded {
        name: manifest.name.clone(),
        source: path.display().to_string(),
        commands: manifest
            .commands
            .iter()
            .map(|command| command.name.clone())
            .collect(),
    };
    Ok((
        Arc::new(ExternalPlugin::from_manifest(
            manifest,
            Some(path.display().to_string()),
        )),
        loaded,
    ))
}

struct ExternalPlugin {
    name: &'static str,
    description: Option<&'static str>,
    version: Option<&'static str>,
    author: Option<&'static str>,
    homepage: Option<&'static str>,
    source: Option<&'static str>,
    commands: &'static [&'static str],
    command_permissions: &'static [(&'static str, &'static str)],
    docs: &'static [CommandDoc],
    specs: HashMap<&'static str, ExternalCommand>,
}

#[derive(Clone)]
struct ExternalCommand {
    run: String,
}

impl ExternalPlugin {
    fn from_manifest(manifest: ExternalManifest, source: Option<String>) -> Self {
        let name = leak_string(manifest.name);
        let description = manifest.description.map(leak_string);
        let version = manifest.version.map(leak_string);
        let author = manifest.author.map(leak_string);
        let homepage = manifest.homepage.map(leak_string);
        let source = source.map(leak_string);
        let mut commands = Vec::new();
        let mut command_permissions = Vec::new();
        let mut docs = Vec::new();
        let mut specs = HashMap::new();

        for command in manifest.commands {
            let command_name = leak_string(command.name);
            let examples = leak_slice(
                command
                    .examples
                    .into_iter()
                    .map(leak_string)
                    .collect::<Vec<_>>(),
            );

            commands.push(command_name);
            command_permissions.push((command_name, "proc.exec"));
            docs.push(CommandDoc {
                command: command_name,
                summary: leak_string(command.summary),
                usage: leak_string(command.usage),
                examples,
            });
            specs.insert(command_name, ExternalCommand { run: command.run });
        }

        Self {
            name,
            description,
            version,
            author,
            homepage,
            source,
            commands: leak_slice(commands),
            command_permissions: leak_slice(command_permissions),
            docs: leak_slice(docs),
            specs,
        }
    }
}

impl ZenPlugin for ExternalPlugin {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> Option<&'static str> {
        self.description
    }

    fn version(&self) -> Option<&'static str> {
        self.version
    }

    fn author(&self) -> Option<&'static str> {
        self.author
    }

    fn homepage(&self) -> Option<&'static str> {
        self.homepage
    }

    fn kind(&self) -> &'static str {
        "external"
    }

    fn source(&self) -> Option<&'static str> {
        self.source
    }

    fn commands(&self) -> &'static [&'static str] {
        self.commands
    }

    fn command_permissions(&self) -> &'static [(&'static str, &'static str)] {
        self.command_permissions
    }

    fn command_docs(&self) -> &'static [CommandDoc] {
        self.docs
    }

    fn call(
        &self,
        executor: &mut dyn PluginHost,
        call: &FunctionCall,
        _input: &Value,
    ) -> Result<PluginResult, String> {
        let command_name = call.name.join(".");
        let Some(command) = self.specs.get(command_name.as_str()) else {
            return Ok(PluginResult::unhandled());
        };

        executor
            .external_process_exec(&command.run, call)
            .map(PluginResult::handled)
    }
}

struct ExternalManifest {
    name: String,
    description: Option<String>,
    version: Option<String>,
    author: Option<String>,
    homepage: Option<String>,
    commands: Vec<ExternalCommandManifestFinal>,
}

#[derive(Default)]
struct ExternalCommandManifest {
    name: String,
    run: String,
    summary: Option<String>,
    usage: Option<String>,
    examples: Vec<String>,
}

fn parse_manifest(text: &str) -> Result<ExternalManifest, String> {
    let mut plugin_name = None;
    let mut description = None;
    let mut version = None;
    let mut author = None;
    let mut homepage = None;
    let mut commands = Vec::new();
    let mut current_command: Option<ExternalCommandManifest> = None;

    for (index, raw_line) in text.lines().enumerate() {
        let line_number = index + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if matches!(line, "[[commands]]" | "[[command]]") {
            if let Some(command) = current_command.take() {
                commands.push(finalize_command(command)?);
            }
            current_command = Some(ExternalCommandManifest::default());
            continue;
        }

        let (key, raw_value) = line
            .split_once('=')
            .ok_or_else(|| format!("line {}: expected key = value", line_number))?;
        let key = key.trim();
        let raw_value = raw_value.trim();

        if let Some(command) = current_command.as_mut() {
            match key {
                "name" => command.name = parse_string(raw_value, line_number)?,
                "run" | "exec" => command.run = parse_string(raw_value, line_number)?,
                "summary" => command.summary = Some(parse_string(raw_value, line_number)?),
                "usage" => command.usage = Some(parse_string(raw_value, line_number)?),
                "examples" => command.examples = parse_string_array(raw_value, line_number)?,
                _ => {
                    return Err(format!(
                        "line {}: unknown command key '{}'",
                        line_number, key
                    ))
                }
            }
        } else {
            match key {
                "name" => plugin_name = Some(parse_string(raw_value, line_number)?),
                "description" | "summary" => {
                    description = Some(parse_string(raw_value, line_number)?)
                }
                "version" => version = Some(parse_string(raw_value, line_number)?),
                "author" => author = Some(parse_string(raw_value, line_number)?),
                "homepage" => homepage = Some(parse_string(raw_value, line_number)?),
                _ => {
                    return Err(format!(
                        "line {}: unknown plugin key '{}'",
                        line_number, key
                    ))
                }
            }
        }
    }

    if let Some(command) = current_command {
        commands.push(finalize_command(command)?);
    }

    let name = plugin_name.ok_or_else(|| "manifest missing plugin name".to_string())?;
    if commands.is_empty() {
        return Err("manifest must define at least one command".into());
    }

    Ok(ExternalManifest {
        name,
        description,
        version,
        author,
        homepage,
        commands,
    })
}

fn finalize_command(
    command: ExternalCommandManifest,
) -> Result<ExternalCommandManifestFinal, String> {
    if command.name.trim().is_empty() {
        return Err("command missing name".into());
    }
    if command.run.trim().is_empty() {
        return Err(format!("command '{}' missing run", command.name));
    }

    let summary = command
        .summary
        .unwrap_or_else(|| "Runs an external command.".into());
    let usage = command.usage.unwrap_or_else(|| command.name.clone());
    let examples = if command.examples.is_empty() {
        vec![command.name.clone()]
    } else {
        command.examples
    };

    Ok(ExternalCommandManifestFinal {
        name: command.name,
        run: command.run,
        summary,
        usage,
        examples,
    })
}

struct ExternalCommandManifestFinal {
    name: String,
    run: String,
    summary: String,
    usage: String,
    examples: Vec<String>,
}

fn parse_string(value: &str, line_number: usize) -> Result<String, String> {
    let value = value.trim();
    if !value.starts_with('"') || !value.ends_with('"') || value.len() < 2 {
        return Err(format!("line {}: expected quoted string", line_number));
    }

    Ok(value[1..value.len() - 1]
        .replace("\\\"", "\"")
        .replace("\\\\", "\\"))
}

fn parse_string_array(value: &str, line_number: usize) -> Result<Vec<String>, String> {
    let value = value.trim();
    if !value.starts_with('[') || !value.ends_with(']') {
        return Err(format!("line {}: expected string array", line_number));
    }

    let inner = value[1..value.len() - 1].trim();
    if inner.is_empty() {
        return Ok(Vec::new());
    }

    inner
        .split(',')
        .map(|item| parse_string(item.trim(), line_number))
        .collect()
}

fn leak_string(value: String) -> &'static str {
    Box::leak(value.into_boxed_str())
}

fn leak_slice<T>(value: Vec<T>) -> &'static [T] {
    Box::leak(value.into_boxed_slice())
}

fn diagnostics_store() -> &'static Mutex<ExternalPluginDiagnostics> {
    DIAGNOSTICS.get_or_init(|| Mutex::new(ExternalPluginDiagnostics::default()))
}

fn record_external_diagnostics(diagnostics: ExternalPluginDiagnostics) {
    if let Ok(mut stored) = diagnostics_store().lock() {
        *stored = diagnostics;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Expr, FunctionCall};
    use crate::permissions::PermissionSet;

    #[test]
    fn manifest_parser_builds_command_defaults() {
        let manifest = parse_manifest(
            r#"
name = "hello"

[[commands]]
name = "hello.world"
run = "echo hello"
"#,
        )
        .unwrap();

        assert_eq!(manifest.name, "hello");
        assert_eq!(manifest.commands.len(), 1);
        assert_eq!(manifest.commands[0].name, "hello.world");
        assert_eq!(manifest.commands[0].summary, "Runs an external command.");
        assert_eq!(manifest.commands[0].usage, "hello.world");
        assert_eq!(manifest.commands[0].examples, vec!["hello.world"]);
    }

    #[test]
    fn manifest_parser_accepts_docs() {
        let manifest = parse_manifest(
            r#"
name = "tools"

[[commands]]
name = "tools.say"
run = "echo"
summary = "Prints text."
usage = "tools.say VALUE"
examples = ["tools.say hello", "tools.say \"hello world\""]
"#,
        )
        .unwrap();

        assert_eq!(manifest.commands[0].summary, "Prints text.");
        assert_eq!(manifest.commands[0].usage, "tools.say VALUE");
        assert_eq!(
            manifest.commands[0].examples,
            vec!["tools.say hello", "tools.say \"hello world\""]
        );
    }

    #[test]
    fn manifest_parser_accepts_plugin_metadata() {
        let manifest = parse_manifest(
            r#"
name = "tools"
description = "Useful workspace tools."
version = "1.2.3"
author = "Zen Team"
homepage = "https://example.com/tools"

[[commands]]
name = "tools.say"
run = "echo"
"#,
        )
        .unwrap();

        assert_eq!(
            manifest.description.as_deref(),
            Some("Useful workspace tools.")
        );
        assert_eq!(manifest.version.as_deref(), Some("1.2.3"));
        assert_eq!(manifest.author.as_deref(), Some("Zen Team"));
        assert_eq!(
            manifest.homepage.as_deref(),
            Some("https://example.com/tools")
        );
    }

    #[test]
    fn load_external_plugins_reads_plugin_directories() {
        let root =
            std::env::temp_dir().join(format!("zen-external-plugin-test-{}", std::process::id()));
        let plugin_dir = root.join("hello");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::write(
            plugin_dir.join("plugin.toml"),
            r#"
name = "hello"
description = "Says hello."
version = "0.1.0"
author = "Zen Tests"

[[commands]]
name = "hello.world"
run = "echo hello"
summary = "Prints hello."
usage = "hello.world"
examples = ["hello.world"]
"#,
        )
        .unwrap();

        let plugins = load_external_plugins_from(&root);
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name(), "hello");
        assert_eq!(plugins[0].description(), Some("Says hello."));
        assert_eq!(plugins[0].version(), Some("0.1.0"));
        assert_eq!(plugins[0].author(), Some("Zen Tests"));
        assert_eq!(plugins[0].kind(), "external");
        assert!(plugins[0].source().unwrap().ends_with("plugin.toml"));
        assert_eq!(plugins[0].commands(), &["hello.world"]);
        assert_eq!(
            plugins[0].command_permissions(),
            &[("hello.world", "proc.exec")]
        );
        assert_eq!(plugins[0].command_docs()[0].summary, "Prints hello.");

        let diagnostics = external_plugin_diagnostics();
        assert_eq!(diagnostics.root, Some(root.display().to_string()));
        assert_eq!(diagnostics.loaded.len(), 1);
        assert_eq!(diagnostics.loaded[0].name, "hello");
        assert_eq!(diagnostics.loaded[0].commands, vec!["hello.world"]);
        assert!(diagnostics.failed.is_empty());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn load_external_plugins_records_manifest_failures() {
        let root = std::env::temp_dir().join(format!(
            "zen-external-plugin-failure-test-{}",
            std::process::id()
        ));
        let plugin_dir = root.join("broken");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::write(plugin_dir.join("plugin.toml"), "name = \"broken\"\n").unwrap();

        let plugins = load_external_plugins_from(&root);
        assert!(plugins.is_empty());

        let diagnostics = external_plugin_diagnostics();
        assert_eq!(diagnostics.root, Some(root.display().to_string()));
        assert!(diagnostics.loaded.is_empty());
        assert_eq!(diagnostics.failed.len(), 1);
        assert!(diagnostics.failed[0].source.ends_with("plugin.toml"));
        assert!(diagnostics.failed[0]
            .error
            .contains("manifest must define at least one command"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn external_plugin_executes_matching_command() {
        let _guard = crate::interrupt::lock_for_test();
        let manifest = parse_manifest(
            r#"
name = "hello"

[[commands]]
name = "hello.world"
run = "echo external"
"#,
        )
        .unwrap();
        let plugin = ExternalPlugin::from_manifest(manifest, None);
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&vec![(
            "proc".into(),
            "exec".into(),
        )]));

        let result = plugin
            .call(
                &mut executor,
                &FunctionCall {
                    name: vec!["hello".into(), "world".into()],
                    args: vec![Expr::string("plugin")],
                    config: None,
                },
                &Value::Null,
            )
            .unwrap();

        let PluginResult::Handled(Value::Object(output)) = result else {
            panic!("expected handled exec output");
        };
        let stdout = output
            .get("stdout")
            .and_then(Value::as_string)
            .unwrap_or_default();
        assert!(stdout.contains("external"));
        assert!(stdout.contains("plugin"));
    }
}
