use crate::ast::{CallConfig, FunctionCall};
#[cfg(test)]
use crate::runtime::executor::Executor;
use crate::runtime::plugin::{CommandDoc, PluginHost, PluginResult, ZenPlugin};
use crate::runtime::values::Value;
use std::collections::HashMap;
use std::ffi::c_void;
use std::io::{self, Write};
use std::ptr;
use zen_runtime::values::{secret_reference_name, value_to_echo_string};

pub struct SecretsPlugin;

const APP_NAMESPACE: &str = "zen";
const DEFAULT_PROFILE: &str = "default";
const LEGACY_TARGET_PREFIX: &str = "zen/";

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

pub fn read_secret(name: &str) -> Result<Option<String>, String> {
    validate_name(name)?;
    if let Some(secret) = credential_read_target(&target_name(name))? {
        return Ok(Some(secret));
    }
    credential_read_target(&legacy_target_name(name))
}

pub fn write_secret(name: &str, secret: &str) -> Result<(), String> {
    validate_name(name)?;
    credential_write_target(&target_name(name), secret)
}

#[cfg(test)]
pub(crate) fn delete_secret(name: &str) -> Result<bool, String> {
    validate_name(name)?;
    let deleted =
        credential_delete_target(&target_name(name))? || credential_delete_target(&legacy_target_name(name))?;
    Ok(deleted)
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
    let deleted = credential_delete_target(&target_name(&name))?
        || credential_delete_target(&legacy_target_name(&name))?;

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
        credential_list_targets()?
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

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("secret name cannot be empty".into());
    }
    if name.contains('/') || name.contains('\\') {
        return Err("secret name cannot contain path separators".into());
    }
    if name.split('.').any(str::is_empty) {
        return Err("secret name segments cannot be empty".into());
    }
    Ok(())
}

fn target_name(name: &str) -> String {
    let path = name.replace('.', "/");
    format!("{}/{}/{}", APP_NAMESPACE, DEFAULT_PROFILE, path)
}

fn legacy_target_name(name: &str) -> String {
    format!("{}{}", LEGACY_TARGET_PREFIX, name)
}

fn friendly_name(target: &str) -> Option<String> {
    let structured_prefix = format!("{}/{}/", APP_NAMESPACE, DEFAULT_PROFILE);
    if let Some(name) = target.strip_prefix(&structured_prefix) {
        return Some(name.replace('/', "."));
    }

    target
        .strip_prefix(LEGACY_TARGET_PREFIX)
        .map(str::to_string)
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
fn credential_write_target(target: &str, secret: &str) -> Result<(), String> {
    let mut target_w = wide_null(target);
    let mut user_w = wide_null("zen");
    let mut blob = secret.as_bytes().to_vec();
    let credential = CredentialW {
        flags: 0,
        type_: CRED_TYPE_GENERIC,
        target_name: target_w.as_mut_ptr(),
        comment: ptr::null_mut(),
        last_written: FileTime {
            dw_low_date_time: 0,
            dw_high_date_time: 0,
        },
        credential_blob_size: blob.len() as u32,
        credential_blob: blob.as_mut_ptr(),
        persist: CRED_PERSIST_LOCAL_MACHINE,
        attribute_count: 0,
        attributes: ptr::null_mut(),
        target_alias: ptr::null_mut(),
        user_name: user_w.as_mut_ptr(),
    };

    let ok = unsafe { CredWriteW(&credential, 0) };
    if ok == 0 {
        Err(format!(
            "Failed to save secret '{}': {}",
            target,
            io::Error::last_os_error()
        ))
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn credential_write_target(_target: &str, _secret: &str) -> Result<(), String> {
    Err("secrets are only supported on Windows Credential Manager".into())
}

#[cfg(windows)]
fn credential_read_target(target: &str) -> Result<Option<String>, String> {
    let target_w = wide_null(target);
    let mut credential: *mut CredentialW = ptr::null_mut();
    let ok = unsafe { CredReadW(target_w.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) };
    if ok == 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(ERROR_NOT_FOUND) {
            return Ok(None);
        }
        return Err(format!("Failed to read secret '{}': {}", target, err));
    }

    let result = unsafe {
        let cred = &*credential;
        let bytes =
            std::slice::from_raw_parts(cred.credential_blob, cred.credential_blob_size as usize);
        String::from_utf8(bytes.to_vec())
            .map(Some)
            .map_err(|_| format!("Secret '{}' is not valid UTF-8", target))
    };
    unsafe { CredFree(credential.cast()) };
    result
}

#[cfg(not(windows))]
fn credential_read_target(_target: &str) -> Result<Option<String>, String> {
    Err("secrets are only supported on Windows Credential Manager".into())
}

#[cfg(windows)]
fn credential_delete_target(target: &str) -> Result<bool, String> {
    let target_w = wide_null(target);
    let ok = unsafe { CredDeleteW(target_w.as_ptr(), CRED_TYPE_GENERIC, 0) };
    if ok == 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(ERROR_NOT_FOUND) {
            return Ok(false);
        }
        Err(format!("Failed to delete secret '{}': {}", target, err))
    } else {
        Ok(true)
    }
}

#[cfg(not(windows))]
fn credential_delete_target(_target: &str) -> Result<bool, String> {
    Err("secrets are only supported on Windows Credential Manager".into())
}

#[cfg(windows)]
fn credential_list_targets() -> Result<Vec<String>, String> {
    let filter = wide_null(&format!("{}/*", APP_NAMESPACE));
    let mut count = 0u32;
    let mut credentials: *mut *mut CredentialW = ptr::null_mut();
    let ok = unsafe { CredEnumerateW(filter.as_ptr(), 0, &mut count, &mut credentials) };
    if ok == 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(ERROR_NOT_FOUND) {
            return Ok(Vec::new());
        }
        return Err(format!("Failed to list secrets: {}", err));
    }

    let mut names = Vec::new();
    unsafe {
        let slice = std::slice::from_raw_parts(credentials, count as usize);
        for credential in slice {
            let target = read_wide_null((**credential).target_name);
            if let Some(name) = friendly_name(&target) {
                names.push(name);
            }
        }
        CredFree(credentials.cast());
    }
    names.sort();
    names.dedup();
    Ok(names)
}

#[cfg(not(windows))]
fn credential_list_targets() -> Result<Vec<String>, String> {
    Err("secrets are only supported on Windows Credential Manager".into())
}

#[cfg(windows)]
fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
unsafe fn read_wide_null(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while *ptr.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
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
const CRED_TYPE_GENERIC: u32 = 1;
#[cfg(windows)]
const CRED_PERSIST_LOCAL_MACHINE: u32 = 2;
#[cfg(windows)]
const ERROR_NOT_FOUND: i32 = 1168;
#[cfg(windows)]
const STD_INPUT_HANDLE: u32 = -10i32 as u32;
#[cfg(windows)]
const ENABLE_ECHO_INPUT: u32 = 0x0004;
#[cfg(windows)]
const INVALID_HANDLE_VALUE: *mut c_void = -1isize as *mut c_void;

#[cfg(windows)]
#[repr(C)]
struct FileTime {
    dw_low_date_time: u32,
    dw_high_date_time: u32,
}

#[cfg(windows)]
#[repr(C)]
struct CredentialAttributeW {
    keyword: *mut u16,
    flags: u32,
    value_size: u32,
    value: *mut u8,
}

#[cfg(windows)]
#[repr(C)]
struct CredentialW {
    flags: u32,
    type_: u32,
    target_name: *mut u16,
    comment: *mut u16,
    last_written: FileTime,
    credential_blob_size: u32,
    credential_blob: *mut u8,
    persist: u32,
    attribute_count: u32,
    attributes: *mut CredentialAttributeW,
    target_alias: *mut u16,
    user_name: *mut u16,
}

#[cfg(windows)]
#[link(name = "Advapi32")]
extern "system" {
    fn CredWriteW(credential: *const CredentialW, flags: u32) -> i32;
    fn CredReadW(
        target_name: *const u16,
        type_: u32,
        flags: u32,
        credential: *mut *mut CredentialW,
    ) -> i32;
    fn CredDeleteW(target_name: *const u16, type_: u32, flags: u32) -> i32;
    fn CredEnumerateW(
        filter: *const u16,
        flags: u32,
        count: *mut u32,
        credentials: *mut *mut *mut CredentialW,
    ) -> i32;
    fn CredFree(buffer: *mut c_void);
}

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

    #[test]
    fn target_names_are_namespaced() {
        assert_eq!(
            target_name("dropbox.refresh_token"),
            "zen/default/dropbox/refresh_token"
        );
    }

    #[test]
    fn legacy_target_names_remain_available_for_reads() {
        assert_eq!(
            legacy_target_name("dropbox.refresh_token"),
            "zen/dropbox.refresh_token"
        );
    }

    #[test]
    fn friendly_names_hide_internal_profile_path() {
        assert_eq!(
            friendly_name("zen/default/dropbox/refresh_token").as_deref(),
            Some("dropbox.refresh_token")
        );
        assert_eq!(
            friendly_name("zen/dropbox.refresh_token").as_deref(),
            Some("dropbox.refresh_token")
        );
    }

    #[test]
    fn secret_names_reject_path_separators() {
        assert!(validate_name("dropbox.refresh_token").is_ok());
        assert!(validate_name("dropbox..refresh_token").is_err());
        assert!(validate_name("dropbox/refresh_token").is_err());
        assert!(validate_name("dropbox\\refresh_token").is_err());
    }
}
