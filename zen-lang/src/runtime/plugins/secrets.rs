use crate::ast::{CallConfig, FunctionCall};
#[cfg(test)]
use crate::runtime::executor::Executor;
use crate::runtime::plugin::{CommandDoc, PluginHost, PluginResult, ZenPlugin};
use crate::runtime::values::Value;
use std::collections::HashMap;
use std::ffi::c_void;
use std::io::{self, Write};
use zen_runtime::secret_store::{delete_secret, list_secrets, read_secret, validate_name, write_secret};
use zen_runtime::values::{secret_reference_name, value_to_echo_string};

pub struct SecretsPlugin;

static SECRETS_DOCS: &[CommandDoc] = &[
    CommandDoc {
        command: "secrets.set",
        summary: "Prompts for a secret and stores it in Windows Credential Manager.",
        usage: "secrets.set <name>",
        examples: &["secrets.set \"dropbox.refresh_token\""],
    },
    CommandDoc {
        command: "secrets.get",
        summary: "Reads a secret from Windows Credential Manager as a redacted value.",
        usage: "secrets.get <name>",
        examples: &["let refresh = secrets.get \"dropbox.refresh_token\""],
    },
    CommandDoc {
        command: "secrets.exists",
        summary: "Checks whether a named secret exists.",
        usage: "secrets.exists <name>",
        examples: &["secrets.exists \"dropbox.refresh_token\""],
    },
    CommandDoc {
        command: "secrets.delete",
        summary: "Deletes a named secret.",
        usage: "secrets.delete <name>",
        examples: &["secrets.delete \"dropbox.refresh_token\""],
    },
    CommandDoc {
        command: "secrets.list",
        summary: "Lists secret names without revealing values.",
        usage: "secrets.list",
        examples: &["secrets.list", "secrets.list | echo table"],
    },
    CommandDoc {
        command: "secrets.save",
        summary: "Bulk-saves named secrets from an object of name/value pairs.",
        usage: "secrets.save <{ \"name\": value, ... }>",
        examples: &["secrets.save { \"pg.password\": $pass, \"pg.user\": $user }"],
    },
];

impl ZenPlugin for SecretsPlugin {
    fn name(&self) -> &'static str {
        "secrets"
    }

    fn commands(&self) -> &'static [&'static str] {
        &[
            "secrets.set",
            "secrets.get",
            "secrets.exists",
            "secrets.delete",
            "secrets.list",
            "secrets.save",
        ]
    }

    fn command_permissions(&self) -> &'static [(&'static str, &'static str)] {
        &[
            ("secrets.set", "secrets.write"),
            ("secrets.get", "secrets.read"),
            ("secrets.exists", "secrets.read"),
            ("secrets.delete", "secrets.write"),
            ("secrets.list", "secrets.read"),
            ("secrets.save", "secrets.write"),
        ]
    }

    fn command_docs(&self) -> &'static [CommandDoc] {
        SECRETS_DOCS
    }

    fn call(
        &self,
        executor: &mut dyn PluginHost,
        call: &FunctionCall,
        _input: &Value,
    ) -> Result<PluginResult, String> {
        match call.name.join(".").as_str() {
            "secrets.set" => secrets_set(executor, call).map(PluginResult::handled),
            "secrets.get" => secrets_get(executor, call).map(PluginResult::handled),
            "secrets.exists" => secrets_exists(executor, call).map(PluginResult::handled),
            "secrets.delete" => secrets_delete(executor, call).map(PluginResult::handled),
            "secrets.list" => secrets_list(executor, call).map(PluginResult::handled),
            "secrets.save" => secrets_save(executor, call).map(PluginResult::handled),
            _ => Ok(PluginResult::unhandled()),
        }
    }
}

fn secrets_set(executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    executor.check_permission("secrets.write")?;
    let name = single_name_arg(executor, call, "secrets.set")?;
    print!("Secret value for '{}': ", name);
    io::stdout()
        .flush()
        .map_err(|e| format!("Failed to flush prompt: {}", e))?;
    let secret = read_secret_from_terminal()?;
    write_secret(&name, &secret)?;

    let mut map = std::collections::HashMap::new();
    map.insert("name".into(), Value::String(name));
    map.insert("saved".into(), Value::Bool(true));
    Ok(Value::Object(map))
}

fn secrets_get(executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    executor.check_permission("secrets.read")?;
    let name = single_name_arg(executor, call, "secrets.get")?;
    match read_secret(&name)? {
        Some(secret) => Ok(Value::Secret(secret)),
        None => Ok(Value::Null),
    }
}

fn secrets_exists(executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    executor.check_permission("secrets.read")?;
    let name = single_name_arg(executor, call, "secrets.exists")?;
    Ok(Value::Bool(read_secret(&name)?.is_some()))
}

fn secrets_delete(executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    executor.check_permission("secrets.write")?;
    let name = single_name_arg(executor, call, "secrets.delete")?;
    let deleted = delete_secret(&name)?;

    let mut map = std::collections::HashMap::new();
    map.insert("name".into(), Value::String(name));
    map.insert("deleted".into(), Value::Bool(deleted));
    Ok(Value::Object(map))
}

fn secrets_list(executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    executor.check_permission("secrets.read")?;
    if !call.args.is_empty() {
        return Err("secrets.list expects no arguments".into());
    }

    Ok(Value::List(
        list_secrets()?
            .into_iter()
            .map(|name| {
                let mut map = std::collections::HashMap::new();
                map.insert("name".into(), Value::String(name));
                Value::Object(map)
            })
            .collect(),
    ))
}

fn secrets_save(executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    executor.check_permission("secrets.write")?;
    // Takes a plain object-literal argument, not a `{ env: {...} }` call
    // config block - call-config env keys must be bare identifiers (no
    // dots), but secret names conventionally contain them (e.g.
    // "dropbox.refresh_token", matching what `secrets.set`/`secrets.get`
    // already accept as a quoted string name).
    let [arg] = call.args.as_slice() else {
        return Err("secrets.save expects one object of { \"name\": value }".into());
    };
    let value = executor.plugin_arg_value(arg.clone())?;
    let Value::Object(map) = value else {
        return Err("secrets.save expects an object of { \"name\": value }".into());
    };

    let mut pairs = Vec::new();
    for (name, value) in map {
        validate_name(&name)?;
        let secret = match value {
            Value::String(value) | Value::Secret(value) => value,
            other => value_to_echo_string(other),
        };
        pairs.push((name, secret));
    }

    save_secrets(pairs)
}

/// Writes each `(name, value)` pair to the secret store and returns a
/// `{ saved, names }` summary. Shared by `secrets.save` and
/// `dropbox.secrets.save` so provider-specific plugins only need to own their
/// own env-var aliasing/validation, not the write loop itself.
pub(crate) fn save_secrets(
    pairs: impl IntoIterator<Item = (String, String)>,
) -> Result<Value, String> {
    let mut names = Vec::new();
    for (name, value) in pairs {
        write_secret(&name, &value)?;
        names.push(name);
    }

    let mut map = HashMap::new();
    map.insert("saved".into(), Value::Number(names.len() as f64));
    map.insert(
        "names".into(),
        Value::List(names.into_iter().map(Value::String).collect()),
    );
    Ok(Value::Object(map))
}

/// Evaluates an `env: {}` call config into a plain env map, resolving any
/// `{ secret: "name" }` reference against the secret store (requiring
/// `secrets.read`) and collecting the resolved plaintext alongside the map so
/// callers building an `ExecRequest` can mask it out of captured process
/// output. Shared by the `exec`/external-command builtins and any plugin
/// (e.g. `postgres.rs`) that spawns a subprocess from a call's env config.
pub fn resolve_env_config(
    executor: &mut dyn PluginHost,
    config: Option<CallConfig>,
) -> Result<(HashMap<String, String>, Vec<String>), String> {
    let mut env = HashMap::new();
    let mut secret_values = Vec::new();

    if let Some(config) = config {
        for (key, expr) in config.env {
            let value = executor.plugin_arg_value(expr)?;
            match secret_reference_name(&value) {
                Some(name) => {
                    executor.check_permission("secrets.read")?;
                    let secret = read_secret(name)?
                        .ok_or_else(|| format!("Secret '{}' was not found", name))?;
                    secret_values.push(secret.clone());
                    env.insert(key, secret);
                }
                None => {
                    env.insert(key, value_to_echo_string(value));
                }
            }
        }
    }

    Ok((env, secret_values))
}

fn single_name_arg(
    executor: &mut dyn PluginHost,
    call: &FunctionCall,
    command: &str,
) -> Result<String, String> {
    let [arg] = call.args.as_slice() else {
        return Err(format!("{} expects <name>", command));
    };
    let value = executor.plugin_arg_value(arg.clone())?;
    let name = match value {
        Value::String(value) | Value::Secret(value) => value,
        Value::Null => "null".into(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        other => format!("{:?}", other),
    };
    validate_name(&name)?;
    Ok(name)
}

#[cfg(windows)]
fn read_secret_from_terminal() -> Result<String, String> {
    let mut guard = ConsoleEchoGuard::disable()?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|e| format!("Failed to read secret: {}", e))?;
    guard.restore();
    println!();
    Ok(input.trim_end_matches(&['\r', '\n'][..]).to_string())
}

#[cfg(not(windows))]
fn read_secret_from_terminal() -> Result<String, String> {
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|e| format!("Failed to read secret: {}", e))?;
    Ok(input.trim_end_matches(&['\r', '\n'][..]).to_string())
}

#[cfg(windows)]
struct ConsoleEchoGuard {
    handle: *mut c_void,
    mode: u32,
    active: bool,
}

#[cfg(windows)]
impl ConsoleEchoGuard {
    fn disable() -> Result<Self, String> {
        let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        if handle == INVALID_HANDLE_VALUE {
            return Err(format!(
                "Failed to read console handle: {}",
                io::Error::last_os_error()
            ));
        }

        let mut mode = 0u32;
        let ok = unsafe { GetConsoleMode(handle, &mut mode) };
        if ok == 0 {
            return Ok(Self {
                handle,
                mode,
                active: false,
            });
        }

        let ok = unsafe { SetConsoleMode(handle, mode & !ENABLE_ECHO_INPUT) };
        if ok == 0 {
            return Err(format!(
                "Failed to disable console echo: {}",
                io::Error::last_os_error()
            ));
        }

        Ok(Self {
            handle,
            mode,
            active: true,
        })
    }

    fn restore(&mut self) {
        if self.active {
            unsafe {
                SetConsoleMode(self.handle, self.mode);
            }
            self.active = false;
        }
    }
}

#[cfg(windows)]
impl Drop for ConsoleEchoGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

#[cfg(windows)]
const STD_INPUT_HANDLE: u32 = -10i32 as u32;
#[cfg(windows)]
const ENABLE_ECHO_INPUT: u32 = 0x0004;
#[cfg(windows)]
const INVALID_HANDLE_VALUE: *mut c_void = -1isize as *mut c_void;

#[cfg(windows)]
#[link(name = "Kernel32")]
extern "system" {
    fn GetStdHandle(std_handle: u32) -> *mut c_void;
    fn GetConsoleMode(console_handle: *mut c_void, mode: *mut u32) -> i32;
    fn SetConsoleMode(console_handle: *mut c_void, mode: u32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::PermissionSet;

    #[test]
    fn secrets_plugin_ignores_unknown_calls() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = SecretsPlugin
            .call(
                &mut executor,
                &FunctionCall {
                    name: vec!["not_secrets".into()],
                    args: Vec::new(),
                    config: None,
                },
                &Value::Null,
            )
            .unwrap();

        assert!(!result.is_handled());
    }

    #[test]
    fn secrets_get_requires_read_permission() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = SecretsPlugin.call(
            &mut executor,
            &FunctionCall {
                name: vec!["secrets".into(), "get".into()],
                args: vec![crate::ast::Expr::String("dropbox.refresh_token".into())],
                config: None,
            },
            &Value::Null,
        );

        match result {
            Err(err) => assert!(err.contains("secrets.read")),
            Ok(_) => panic!("Expected secrets.get to require secrets.read"),
        }
    }

    #[test]
    fn secrets_save_requires_write_permission() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = SecretsPlugin.call(
            &mut executor,
            &FunctionCall {
                name: vec!["secrets".into(), "save".into()],
                args: Vec::new(),
                config: None,
            },
            &Value::Null,
        );

        match result {
            Err(err) => assert!(err.contains("secrets.write")),
            Ok(_) => panic!("Expected secrets.save to require secrets.write"),
        }
    }

    #[test]
    fn secrets_save_writes_multiple_secrets_and_reports_names() {
        let pid = std::process::id();
        let name_a = format!("zen.test.save_a.{}", pid);
        let name_b = format!("zen.test.save_b.{}", pid);

        let mut executor = Executor::new_with_permissions(PermissionSet::new(&vec![(
            "secrets".into(),
            "write".into(),
        )]));
        let result = SecretsPlugin
            .call(
                &mut executor,
                &FunctionCall {
                    name: vec!["secrets".into(), "save".into()],
                    args: vec![crate::ast::Expr::Object(vec![
                        (name_a.clone(), crate::ast::Expr::string("value-a")),
                        (name_b.clone(), crate::ast::Expr::string("value-b")),
                    ])],
                    config: None,
                },
                &Value::Null,
            )
            .unwrap();

        let read_a = read_secret(&name_a).unwrap();
        let read_b = read_secret(&name_b).unwrap();
        delete_secret(&name_a).unwrap();
        delete_secret(&name_b).unwrap();

        assert_eq!(read_a.as_deref(), Some("value-a"));
        assert_eq!(read_b.as_deref(), Some("value-b"));

        let PluginResult::Handled(Value::Object(map)) = result else {
            panic!("Expected secrets.save result object");
        };
        assert!(matches!(map.get("saved"), Some(Value::Number(n)) if *n == 2.0));
        let Some(Value::List(names)) = map.get("names") else {
            panic!("Expected names list");
        };
        let names: Vec<&str> = names.iter().filter_map(Value::as_string).collect();
        assert!(names.contains(&name_a.as_str()));
        assert!(names.contains(&name_b.as_str()));
    }

    #[test]
    fn resolve_env_config_requires_secrets_read_permission() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&vec![(
            "proc".into(),
            "exec".into(),
        )]));
        let config = CallConfig {
            env: vec![(
                "SECRET_ENV".into(),
                crate::ast::Expr::Object(vec![(
                    "secret".into(),
                    crate::ast::Expr::string("zen.test.missing"),
                )]),
            )],
        };

        match resolve_env_config(&mut executor, Some(config)) {
            Err(err) => assert!(err.contains("secrets.read")),
            Ok(_) => panic!("Expected resolve_env_config to require secrets.read"),
        }
    }

    #[test]
    fn resolve_env_config_resolves_secret_reference_and_collects_it_for_masking() {
        let name = format!("zen.test.resolve_env.{}", std::process::id());
        write_secret(&name, "resolved-plaintext").unwrap();

        let mut executor = Executor::new_with_permissions(PermissionSet::new(&vec![
            ("proc".into(), "exec".into()),
            ("secrets".into(), "read".into()),
        ]));
        let config = CallConfig {
            env: vec![(
                "SECRET_ENV".into(),
                crate::ast::Expr::Object(vec![(
                    "secret".into(),
                    crate::ast::Expr::string(name.clone()),
                )]),
            )],
        };

        let result = resolve_env_config(&mut executor, Some(config));
        delete_secret(&name).unwrap();
        let (env, secret_values) = result.unwrap();

        assert_eq!(env.get("SECRET_ENV").map(String::as_str), Some("resolved-plaintext"));
        assert_eq!(secret_values, vec!["resolved-plaintext".to_string()]);
    }
}
