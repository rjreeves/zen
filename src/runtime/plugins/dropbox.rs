use crate::ast::{CallConfig, Expr, FunctionCall};
use crate::runtime::executor::Executor;
use crate::runtime::plugin::{CommandDoc, PluginHost, PluginResult, ZenPlugin};
use crate::runtime::plugins::secrets::{read_secret, write_secret};
use crate::runtime::values::Value;
use ring::digest::{digest, SHA256};
use zen_runtime::values::json_to_value;
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

const DROPBOX_CONTENT_HASH_BLOCK_SIZE: usize = 4 * 1024 * 1024;

pub struct DropboxPlugin;

static DROPBOX_DOCS: &[CommandDoc] = &[
    CommandDoc {
        command: "dropbox.status",
        summary: "Checks Dropbox credential configuration and account access.",
        usage: "dropbox.status",
        examples: &["dropbox.status", "dropbox.status | echo table"],
    },
    CommandDoc {
        command: "dropbox.auth.url",
        summary: "Builds a Dropbox OAuth URL for one-time credential setup.",
        usage: "dropbox.auth.url",
        examples: &[
            "dropbox.auth.url { env: { DROPBOX_APP_KEY: $key } }",
            "dropbox.auth.url | select url",
        ],
    },
    CommandDoc {
        command: "dropbox.auth.finish",
        summary: "Exchanges a Dropbox OAuth code and saves returned credentials.",
        usage: "dropbox.auth.finish <code>",
        examples: &[
            "dropbox.auth.finish $code { env: { DROPBOX_APP_KEY: $key } }",
            "dropbox.auth.finish $code { env: { DROPBOX_APP_KEY: $key, DROPBOX_APP_SECRET: $secret } }",
        ],
    },
    CommandDoc {
        command: "dropbox.account",
        summary: "Returns metadata for the authenticated Dropbox account.",
        usage: "dropbox.account",
        examples: &["dropbox.account", "dropbox.account | select email"],
    },
    CommandDoc {
        command: "dropbox.list",
        summary: "Lists files and folders in a Dropbox folder.",
        usage: "dropbox.list <remote-path>",
        examples: &[
            "dropbox.list \"\" { env: { DROPBOX_REFRESH_TOKEN: $refresh, DROPBOX_APP_KEY: $key } }",
            "dropbox.list \"/reports\" | echo table",
        ],
    },
    CommandDoc {
        command: "dropbox.metadata",
        summary: "Returns Dropbox metadata for one file or folder.",
        usage: "dropbox.metadata <remote-path>",
        examples: &[
            "dropbox.metadata \"/reports/q1.csv\"",
            "let meta = dropbox.metadata \"/reports/q1.csv\"",
            "dropbox.verify \"q1.csv\" meta.content_hash",
        ],
    },
    CommandDoc {
        command: "dropbox.download",
        summary: "Downloads a Dropbox file as text or writes it to a local path.",
        usage: "dropbox.download <remote-path> [local-path]",
        examples: &[
            "dropbox.download \"/notes/todo.txt\"",
            "dropbox.download \"/reports/q1.csv\" \"q1.csv\"",
        ],
    },
    CommandDoc {
        command: "dropbox.download.verify",
        summary: "Downloads a Dropbox file and verifies it against Dropbox's content_hash.",
        usage: "dropbox.download.verify <remote-path> <local-path>",
        examples: &["dropbox.download.verify \"/reports/q1.csv\" \"q1.csv\""],
    },
    CommandDoc {
        command: "dropbox.sync.down",
        summary: "Downloads changed files from a Dropbox folder into a local folder.",
        usage: "dropbox.sync.down <remote-folder> <local-folder>",
        examples: &["dropbox.sync.down \"/reports\" \"reports\""],
    },
    CommandDoc {
        command: "dropbox.sync.plan.down",
        summary: "Shows which files would be downloaded by Dropbox sync down.",
        usage: "dropbox.sync.plan.down <remote-folder> <local-folder>",
        examples: &["dropbox.sync.plan.down \"/reports\" \"reports\""],
    },
    CommandDoc {
        command: "dropbox.sync.up",
        summary: "Uploads changed files from a local folder into a Dropbox folder.",
        usage: "dropbox.sync.up <local-folder> <remote-folder>",
        examples: &["dropbox.sync.up \"reports\" \"/reports\""],
    },
    CommandDoc {
        command: "dropbox.sync.plan.up",
        summary: "Shows which files would be uploaded by Dropbox sync up.",
        usage: "dropbox.sync.plan.up <local-folder> <remote-folder>",
        examples: &["dropbox.sync.plan.up \"reports\" \"/reports\""],
    },
    CommandDoc {
        command: "dropbox.secrets.import",
        summary: "Imports a Dropbox JSON secret bundle into Windows Credential Manager.",
        usage: "dropbox.secrets.import <remote-path>",
        examples: &[
            "dropbox.secrets.import \"/zen/secrets.json\"",
            "dropbox.secrets.import \"/zen/bootstrap.json\"",
        ],
    },
    CommandDoc {
        command: "dropbox.secrets.save",
        summary: "Saves Dropbox credentials from env config or process environment.",
        usage: "dropbox.secrets.save",
        examples: &[
            "dropbox.secrets.save { env: { DROPBOX_REFRESH_TOKEN: $refresh, DROPBOX_APP_KEY: $key } }",
            "dropbox.secrets.save { env: { DROPBOX_TOKEN: $token } }",
        ],
    },
    CommandDoc {
        command: "dropbox.upload",
        summary: "Uploads a local file to Dropbox.",
        usage: "dropbox.upload <local-path> <remote-path> [add|rename|overwrite]",
        examples: &[
            "dropbox.upload \"q1.csv\" \"/reports/q1.csv\"",
            "dropbox.upload \"q1.csv\" \"/reports/q1.csv\" rename",
            "dropbox.upload \"q1.csv\" \"/reports/q1.csv\" overwrite",
        ],
    },
    CommandDoc {
        command: "dropbox.upload.verify",
        summary: "Uploads a local file and verifies Dropbox's returned content_hash.",
        usage: "dropbox.upload.verify <local-path> <remote-path> [add|rename|overwrite]",
        examples: &[
            "dropbox.upload.verify \"q1.csv\" \"/reports/q1.csv\"",
            "dropbox.upload.verify \"q1.csv\" \"/reports/q1.csv\" overwrite",
        ],
    },
    CommandDoc {
        command: "dropbox.hash",
        summary: "Computes Dropbox's content_hash for a local file.",
        usage: "dropbox.hash <local-path>",
        examples: &[
            "dropbox.hash \"q1.csv\"",
            "dropbox.hash \"q1.csv\" | select content_hash",
        ],
    },
    CommandDoc {
        command: "dropbox.verify",
        summary: "Compares a local file against a Dropbox content_hash.",
        usage: "dropbox.verify <local-path> <content-hash>",
        examples: &["dropbox.verify \"q1.csv\" \"abc123...\""],
    },
    CommandDoc {
        command: "dropbox.delete",
        summary: "Deletes a Dropbox file or folder.",
        usage: "dropbox.delete <remote-path>",
        examples: &["dropbox.delete \"/reports/old.csv\""],
    },
];

impl ZenPlugin for DropboxPlugin {
    fn name(&self) -> &'static str {
        "dropbox"
    }

    fn commands(&self) -> &'static [&'static str] {
        &[
            "dropbox.status",
            "dropbox.auth.url",
            "dropbox.auth.finish",
            "dropbox.account",
            "dropbox.list",
            "dropbox.metadata",
            "dropbox.download",
            "dropbox.download.verify",
            "dropbox.sync.down",
            "dropbox.sync.plan.down",
            "dropbox.sync.up",
            "dropbox.sync.plan.up",
            "dropbox.secrets.import",
            "dropbox.secrets.save",
            "dropbox.upload",
            "dropbox.upload.verify",
            "dropbox.hash",
            "dropbox.verify",
            "dropbox.delete",
        ]
    }

    fn command_permissions(&self) -> &'static [(&'static str, &'static str)] {
        &[
            ("dropbox.status", "dropbox.read"),
            ("dropbox.auth.finish", "secrets.write"),
            ("dropbox.account", "dropbox.read"),
            ("dropbox.list", "dropbox.read"),
            ("dropbox.metadata", "dropbox.read"),
            ("dropbox.download", "dropbox.read"),
            ("dropbox.download.verify", "dropbox.read"),
            ("dropbox.sync.down", "dropbox.read"),
            ("dropbox.sync.plan.down", "dropbox.read"),
            ("dropbox.sync.up", "dropbox.read"),
            ("dropbox.sync.up", "dropbox.write"),
            ("dropbox.sync.plan.up", "dropbox.read"),
            ("dropbox.secrets.import", "dropbox.read"),
            ("dropbox.secrets.import", "secrets.write"),
            ("dropbox.secrets.save", "secrets.write"),
            ("dropbox.upload", "dropbox.write"),
            ("dropbox.upload.verify", "dropbox.write"),
            ("dropbox.delete", "dropbox.write"),
        ]
    }

    fn command_docs(&self) -> &'static [CommandDoc] {
        DROPBOX_DOCS
    }

    fn call(
        &self,
        executor: &mut dyn PluginHost,
        call: &FunctionCall,
        _input: &Value,
    ) -> Result<PluginResult, String> {
        match call.name.join(".").as_str() {
            "dropbox.status" => dropbox_status(executor, call).map(PluginResult::handled),
            "dropbox.auth.url" => dropbox_auth_url(executor, call).map(PluginResult::handled),
            "dropbox.auth.finish" => dropbox_auth_finish(executor, call).map(PluginResult::handled),
            "dropbox.account" => dropbox_account(executor, call).map(PluginResult::handled),
            "dropbox.list" => dropbox_list(executor, call).map(PluginResult::handled),
            "dropbox.metadata" => dropbox_metadata(executor, call).map(PluginResult::handled),
            "dropbox.download" => dropbox_download(executor, call).map(PluginResult::handled),
            "dropbox.download.verify" => {
                dropbox_download_verify(executor, call).map(PluginResult::handled)
            }
            "dropbox.sync.down" => dropbox_sync_down(executor, call).map(PluginResult::handled),
            "dropbox.sync.plan.down" => {
                dropbox_sync_plan_down(executor, call).map(PluginResult::handled)
            }
            "dropbox.sync.up" => dropbox_sync_up(executor, call).map(PluginResult::handled),
            "dropbox.sync.plan.up" => {
                dropbox_sync_plan_up(executor, call).map(PluginResult::handled)
            }
            "dropbox.secrets.import" => {
                dropbox_secrets_import(executor, call).map(PluginResult::handled)
            }
            "dropbox.secrets.save" => {
                dropbox_secrets_save(executor, call).map(PluginResult::handled)
            }
            "dropbox.upload" => dropbox_upload(executor, call).map(PluginResult::handled),
            "dropbox.upload.verify" => {
                dropbox_upload_verify(executor, call).map(PluginResult::handled)
            }
            "dropbox.hash" => dropbox_hash(executor, call).map(PluginResult::handled),
            "dropbox.verify" => dropbox_verify(executor, call).map(PluginResult::handled),
            "dropbox.delete" => dropbox_delete(executor, call).map(PluginResult::handled),
            _ => Ok(PluginResult::unhandled()),
        }
    }
}

fn dropbox_status(executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    executor.check_permission("dropbox.read")?;
    if !call.args.is_empty() {
        return Err("dropbox.status expects no arguments".into());
    }

    let env = call_env(executor, call.config.clone())?;
    let summary = credential_summary(&env);
    let mut map = HashMap::new();
    map.insert("configured".into(), Value::Bool(summary.configured));
    map.insert("source".into(), Value::String(summary.source.into()));

    match access_token_from_env(env) {
        Ok(token) => match account_json(&token) {
            Ok(account) => {
                map.insert("auth".into(), Value::String("ok".into()));
                if let Some(account_id) = account.get("account_id").and_then(|value| value.as_str())
                {
                    map.insert("account_id".into(), Value::String(account_id.into()));
                }
                if let Some(email) = account.get("email").and_then(|value| value.as_str()) {
                    map.insert("email".into(), Value::String(email.into()));
                }
                if let Some(name) = account
                    .get("name")
                    .and_then(|name| name.get("display_name"))
                    .and_then(|value| value.as_str())
                {
                    map.insert("name".into(), Value::String(name.into()));
                }
            }
            Err(err) => {
                map.insert("auth".into(), Value::String("error".into()));
                map.insert("error".into(), Value::String(err));
            }
        },
        Err(err) => {
            map.insert("auth".into(), Value::String("error".into()));
            map.insert("error".into(), Value::String(err));
        }
    }

    Ok(Value::Object(map))
}

fn dropbox_auth_url(executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    if !call.args.is_empty() {
        return Err("dropbox.auth.url expects no arguments".into());
    }

    let env = call_env(executor, call.config.clone())?;
    let app_key = app_key_from_env_or_secret(&env).ok_or_else(|| {
        "dropbox.auth.url needs DROPBOX_APP_KEY or DROPBOX_CLIENT_ID in env config, process environment, or saved dropbox.app_key"
            .to_string()
    })?;
    let url = dropbox_oauth_url(&app_key);

    let mut map = HashMap::new();
    map.insert("url".into(), Value::String(url));
    map.insert("response_type".into(), Value::String("code".into()));
    map.insert("token_access_type".into(), Value::String("offline".into()));
    Ok(Value::Object(map))
}

fn dropbox_auth_finish(executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    executor.check_permission("secrets.write")?;

    let args = arg_strings(executor, call.args.clone())?;
    let code = match args.as_slice() {
        [code] => code,
        _ => return Err("dropbox.auth.finish expects <code>".into()),
    };
    let env = call_env(executor, call.config.clone())?;
    let configured_app_key = app_key_from_env(&env);
    let app_key = configured_app_key.clone().or_else(|| secret_value("dropbox.app_key")).ok_or_else(|| {
        "dropbox.auth.finish needs DROPBOX_APP_KEY or DROPBOX_CLIENT_ID in env config, process environment, or saved dropbox.app_key"
            .to_string()
    })?;
    let app_secret = app_secret_from_env_or_secret(&env, configured_app_key.is_none());
    let token = finish_oauth_code(code, &app_key, app_secret.as_deref())?;
    let secrets = oauth_secrets(&token, &app_key, app_secret.as_deref())?;

    for (name, value) in &secrets {
        write_secret(name, value)?;
    }

    let mut map = HashMap::new();
    map.insert("saved".into(), Value::Number(secrets.len() as f64));
    map.insert(
        "names".into(),
        Value::List(
            secrets
                .into_iter()
                .map(|(name, _)| Value::String(name))
                .collect(),
        ),
    );
    if let Some(account_id) = token.get("account_id").and_then(|value| value.as_str()) {
        map.insert("account_id".into(), Value::String(account_id.into()));
    }
    if let Some(uid) = token.get("uid").and_then(|value| value.as_str()) {
        map.insert("uid".into(), Value::String(uid.into()));
    }
    Ok(Value::Object(map))
}

fn dropbox_account(executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    executor.check_permission("dropbox.read")?;
    if !call.args.is_empty() {
        return Err("dropbox.account expects no arguments".into());
    }

    let token = access_token(executor, call.config.clone())?;
    account_json(&token).map(json_to_value)
}

fn dropbox_list(executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    executor.check_permission("dropbox.read")?;

    let args = arg_strings(executor, call.args.clone())?;
    let path = match args.as_slice() {
        [path] => path,
        _ => return Err("dropbox.list expects <remote-path>".into()),
    };
    let token = access_token(executor, call.config.clone())?;
    let body = json!({
        "path": path,
        "recursive": false,
        "include_media_info": false,
        "include_deleted": false,
        "include_has_explicit_shared_members": false
    });
    let json = list_folder_all(&token, body)?;

    Ok(json_to_value(json))
}

fn dropbox_metadata(executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    executor.check_permission("dropbox.read")?;

    let args = arg_strings(executor, call.args.clone())?;
    let path = match args.as_slice() {
        [path] => path,
        _ => return Err("dropbox.metadata expects <remote-path>".into()),
    };
    let token = access_token(executor, call.config.clone())?;
    let json = get_metadata(&token, path)?;

    Ok(json_to_value(json))
}

fn get_metadata(token: &str, path: &str) -> Result<serde_json::Value, String> {
    let body = json!({
        "path": path,
        "include_media_info": false,
        "include_deleted": false,
        "include_has_explicit_shared_members": false
    });
    let response = ureq::post("https://api.dropboxapi.com/2/files/get_metadata")
        .set("Authorization", &format!("Bearer {}", token))
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())
        .map_err(describe_ureq_error)?;

    response_json(response)
}

fn list_folder_all(token: &str, body: serde_json::Value) -> Result<serde_json::Value, String> {
    let response = ureq::post("https://api.dropboxapi.com/2/files/list_folder")
        .set("Authorization", &format!("Bearer {}", token))
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())
        .map_err(describe_ureq_error)?;
    let mut page = response_json(response)?;

    while page
        .get("has_more")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        let cursor = page
            .get("cursor")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "Dropbox list response has_more without cursor".to_string())?
            .to_string();
        let next = list_folder_continue(token, &cursor)?;
        merge_list_folder_page(&mut page, next)?;
    }

    Ok(page)
}

fn list_folder_continue(token: &str, cursor: &str) -> Result<serde_json::Value, String> {
    let body = json!({ "cursor": cursor });
    let response = ureq::post("https://api.dropboxapi.com/2/files/list_folder/continue")
        .set("Authorization", &format!("Bearer {}", token))
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())
        .map_err(describe_ureq_error)?;
    response_json(response)
}

fn merge_list_folder_page(
    base: &mut serde_json::Value,
    next: serde_json::Value,
) -> Result<(), String> {
    let base_entries = base
        .get_mut("entries")
        .and_then(|value| value.as_array_mut())
        .ok_or_else(|| "Dropbox list response missing entries array".to_string())?;
    let mut next_entries = next
        .get("entries")
        .and_then(|value| value.as_array())
        .ok_or_else(|| "Dropbox list continuation missing entries array".to_string())?
        .clone();
    base_entries.append(&mut next_entries);

    if let Some(cursor) = next.get("cursor").cloned() {
        base["cursor"] = cursor;
    }
    if let Some(has_more) = next.get("has_more").cloned() {
        base["has_more"] = has_more;
    }

    Ok(())
}

fn dropbox_download(executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    executor.check_permission("dropbox.read")?;

    let args = arg_strings(executor, call.args.clone())?;
    let (remote_path, local_path) = match args.as_slice() {
        [remote_path] => (remote_path, None),
        [remote_path, local_path] => (remote_path, Some(local_path)),
        _ => return Err("dropbox.download expects <remote-path> [local-path]".into()),
    };
    let token = access_token(executor, call.config.clone())?;
    let downloaded = download_file(&token, remote_path)?;

    if let Some(path) = local_path {
        let resolved = executor.resolve_local_write_path(path)?;
        write_downloaded_file(&resolved, &downloaded.bytes)?;
        let mut map = HashMap::new();
        map.insert("path".into(), Value::String(path.clone()));
        map.insert(
            "resolved_path".into(),
            Value::String(resolved.to_string_lossy().into_owned()),
        );
        map.insert("bytes".into(), Value::Number(downloaded.bytes.len() as f64));
        if let Some(metadata) = downloaded.metadata {
            map.insert("metadata".into(), json_to_value(metadata));
        }
        return Ok(Value::Object(map));
    }

    String::from_utf8(downloaded.bytes)
        .map(Value::String)
        .map_err(|_| "dropbox.download without local-path expects UTF-8 file content".into())
}

fn dropbox_download_verify(executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    executor.check_permission("dropbox.read")?;

    let args = arg_strings(executor, call.args.clone())?;
    let (remote_path, local_path) = match args.as_slice() {
        [remote_path, local_path] => (remote_path, local_path),
        _ => return Err("dropbox.download.verify expects <remote-path> <local-path>".into()),
    };
    let token = access_token(executor, call.config.clone())?;
    let downloaded = download_file(&token, remote_path)?;
    let resolved = executor.resolve_local_write_path(local_path)?;
    write_downloaded_file(&resolved, &downloaded.bytes)?;
    let local_hash = dropbox_content_hash(&downloaded.bytes);

    download_verification_result(
        remote_path,
        &resolved.to_string_lossy(),
        downloaded.bytes.len(),
        local_hash,
        downloaded.metadata,
    )
}

fn download_verification_result(
    remote_path: &str,
    local_path: &str,
    byte_count: usize,
    local_hash: String,
    metadata: Option<serde_json::Value>,
) -> Result<Value, String> {
    let metadata = metadata.ok_or_else(|| {
        "Dropbox download response missing metadata needed for content_hash verification"
            .to_string()
    })?;
    let remote_hash = metadata
        .get("content_hash")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "Dropbox download metadata missing content_hash".to_string())?
        .to_string();
    let matches = local_hash.eq_ignore_ascii_case(&remote_hash);

    let mut map = HashMap::new();
    map.insert("path".into(), Value::String(local_path.into()));
    map.insert("remote_path".into(), Value::String(remote_path.into()));
    map.insert("bytes".into(), Value::Number(byte_count as f64));
    map.insert("content_hash".into(), Value::String(local_hash));
    map.insert("remote_content_hash".into(), Value::String(remote_hash));
    map.insert("matches".into(), Value::Bool(matches));
    map.insert("metadata".into(), json_to_value(metadata));
    Ok(Value::Object(map))
}

fn dropbox_sync_down(executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    executor.check_permission("dropbox.read")?;

    let args = arg_strings(executor, call.args.clone())?;
    let (remote_folder, local_folder) = match args.as_slice() {
        [remote_folder, local_folder] => (remote_folder, local_folder),
        _ => return Err("dropbox.sync.down expects <remote-folder> <local-folder>".into()),
    };
    let local_root = executor.resolve_local_write_path(local_folder)?;
    let token = access_token(executor, call.config.clone())?;
    let body = json!({
        "path": remote_folder,
        "recursive": true,
        "include_media_info": false,
        "include_deleted": false,
        "include_has_explicit_shared_members": false
    });
    let listing = list_folder_all(&token, body)?;
    let entries = listing
        .get("entries")
        .and_then(|value| value.as_array())
        .ok_or_else(|| "Dropbox list response missing entries array".to_string())?;

    sync_down_entries(&token, remote_folder, &local_root, entries)
}

fn dropbox_sync_plan_down(executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    executor.check_permission("dropbox.read")?;

    let args = arg_strings(executor, call.args.clone())?;
    let (remote_folder, local_folder) = match args.as_slice() {
        [remote_folder, local_folder] => (remote_folder, local_folder),
        _ => return Err("dropbox.sync.plan.down expects <remote-folder> <local-folder>".into()),
    };
    let local_root = executor.resolve_local_write_path(local_folder)?;
    let token = access_token(executor, call.config.clone())?;
    let body = json!({
        "path": remote_folder,
        "recursive": true,
        "include_media_info": false,
        "include_deleted": false,
        "include_has_explicit_shared_members": false
    });
    let listing = list_folder_all(&token, body)?;
    let entries = listing
        .get("entries")
        .and_then(|value| value.as_array())
        .ok_or_else(|| "Dropbox list response missing entries array".to_string())?;

    plan_down_entries(remote_folder, &local_root, entries)
}

fn dropbox_sync_up(executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    executor.check_permission("dropbox.read")?;
    executor.check_permission("dropbox.write")?;

    let args = arg_strings(executor, call.args.clone())?;
    let (local_folder, remote_folder) = match args.as_slice() {
        [local_folder, remote_folder] => (local_folder, remote_folder),
        _ => return Err("dropbox.sync.up expects <local-folder> <remote-folder>".into()),
    };
    let local_root = executor.resolve_local_write_path(local_folder)?;
    let metadata = fs::metadata(&local_root).map_err(|e| format!("Invalid local folder: {}", e))?;
    if !metadata.is_dir() {
        return Err("dropbox.sync.up expects local-folder to be a directory".into());
    }

    let token = access_token(executor, call.config.clone())?;
    let body = json!({
        "path": remote_folder,
        "recursive": true,
        "include_media_info": false,
        "include_deleted": false,
        "include_has_explicit_shared_members": false
    });
    let listing = match list_folder_all(&token, body) {
        Ok(listing) => listing,
        Err(err) if is_dropbox_path_not_found(&err) => json!({ "entries": [] }),
        Err(err) => return Err(err),
    };
    let entries = listing
        .get("entries")
        .and_then(|value| value.as_array())
        .ok_or_else(|| "Dropbox list response missing entries array".to_string())?;
    let remote_hashes = remote_hashes_by_path(entries)?;
    let local_files = collect_local_files(&local_root)?;

    sync_up_files(
        &token,
        &local_root,
        remote_folder,
        &remote_hashes,
        &local_files,
    )
}

fn dropbox_sync_plan_up(executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    executor.check_permission("dropbox.read")?;

    let args = arg_strings(executor, call.args.clone())?;
    let (local_folder, remote_folder) = match args.as_slice() {
        [local_folder, remote_folder] => (local_folder, remote_folder),
        _ => return Err("dropbox.sync.plan.up expects <local-folder> <remote-folder>".into()),
    };
    let local_root = executor.resolve_local_write_path(local_folder)?;
    let metadata = fs::metadata(&local_root).map_err(|e| format!("Invalid local folder: {}", e))?;
    if !metadata.is_dir() {
        return Err("dropbox.sync.plan.up expects local-folder to be a directory".into());
    }

    let token = access_token(executor, call.config.clone())?;
    let body = json!({
        "path": remote_folder,
        "recursive": true,
        "include_media_info": false,
        "include_deleted": false,
        "include_has_explicit_shared_members": false
    });
    let listing = match list_folder_all(&token, body) {
        Ok(listing) => listing,
        Err(err) if is_dropbox_path_not_found(&err) => json!({ "entries": [] }),
        Err(err) => return Err(err),
    };
    let entries = listing
        .get("entries")
        .and_then(|value| value.as_array())
        .ok_or_else(|| "Dropbox list response missing entries array".to_string())?;
    let remote_hashes = remote_hashes_by_path(entries)?;
    let local_files = collect_local_files(&local_root)?;

    plan_up_files(&local_root, remote_folder, &remote_hashes, &local_files)
}

fn sync_down_entries(
    token: &str,
    remote_folder: &str,
    local_root: &Path,
    entries: &[serde_json::Value],
) -> Result<Value, String> {
    let mut files = 0usize;
    let mut downloaded_count = 0usize;
    let mut skipped = 0usize;
    let mut bytes_written = 0usize;
    let mut results = Vec::new();

    for entry in entries {
        if entry.get(".tag").and_then(|value| value.as_str()) != Some("file") {
            continue;
        }

        files += 1;
        let remote_path = entry
            .get("path_display")
            .or_else(|| entry.get("path_lower"))
            .and_then(|value| value.as_str())
            .ok_or_else(|| "Dropbox file entry missing path".to_string())?;
        let remote_hash = entry
            .get("content_hash")
            .and_then(|value| value.as_str())
            .ok_or_else(|| format!("Dropbox file '{}' missing content_hash", remote_path))?;
        let relative = remote_relative_path(remote_folder, remote_path)?;
        let local_path = safe_join(local_root, &relative)?;

        if local_path.is_file() {
            let local_bytes = fs::read(&local_path)
                .map_err(|e| format!("Failed to read '{}': {}", local_path.display(), e))?;
            let local_hash = dropbox_content_hash(&local_bytes);
            if local_hash.eq_ignore_ascii_case(remote_hash) {
                skipped += 1;
                results.push(sync_entry_result(
                    remote_path,
                    &local_path,
                    local_bytes.len(),
                    local_hash,
                    remote_hash,
                    "skipped",
                ));
                continue;
            }
        }

        let downloaded = download_file(token, remote_path)?;
        write_downloaded_file(&local_path, &downloaded.bytes)?;
        let local_hash = dropbox_content_hash(&downloaded.bytes);
        let matches = local_hash.eq_ignore_ascii_case(remote_hash);
        if !matches {
            return Err(format!(
                "Downloaded '{}' but local content hash did not match Dropbox metadata",
                remote_path
            ));
        }

        downloaded_count += 1;
        bytes_written += downloaded.bytes.len();
        results.push(sync_entry_result(
            remote_path,
            &local_path,
            downloaded.bytes.len(),
            local_hash,
            remote_hash,
            "downloaded",
        ));
    }

    let mut map = HashMap::new();
    map.insert("files".into(), Value::Number(files as f64));
    map.insert("downloaded".into(), Value::Number(downloaded_count as f64));
    map.insert("skipped".into(), Value::Number(skipped as f64));
    map.insert("bytes_written".into(), Value::Number(bytes_written as f64));
    map.insert(
        "local_root".into(),
        Value::String(local_root.to_string_lossy().into_owned()),
    );
    map.insert("entries".into(), Value::List(results));
    Ok(Value::Object(map))
}

fn plan_down_entries(
    remote_folder: &str,
    local_root: &Path,
    entries: &[serde_json::Value],
) -> Result<Value, String> {
    let mut files = 0usize;
    let mut planned_downloads = 0usize;
    let mut skipped = 0usize;
    let mut bytes_to_download = 0usize;
    let mut results = Vec::new();

    for entry in entries {
        if entry.get(".tag").and_then(|value| value.as_str()) != Some("file") {
            continue;
        }

        files += 1;
        let remote_path = entry
            .get("path_display")
            .or_else(|| entry.get("path_lower"))
            .and_then(|value| value.as_str())
            .ok_or_else(|| "Dropbox file entry missing path".to_string())?;
        let remote_hash = entry
            .get("content_hash")
            .and_then(|value| value.as_str())
            .ok_or_else(|| format!("Dropbox file '{}' missing content_hash", remote_path))?;
        let relative = remote_relative_path(remote_folder, remote_path)?;
        let local_path = safe_join(local_root, &relative)?;
        let remote_size = entry
            .get("size")
            .and_then(|value| value.as_u64())
            .unwrap_or(0) as usize;

        if local_path.is_file() {
            let local_bytes = fs::read(&local_path)
                .map_err(|e| format!("Failed to read '{}': {}", local_path.display(), e))?;
            let local_hash = dropbox_content_hash(&local_bytes);
            if local_hash.eq_ignore_ascii_case(remote_hash) {
                skipped += 1;
                results.push(sync_entry_result(
                    remote_path,
                    &local_path,
                    local_bytes.len(),
                    local_hash,
                    remote_hash,
                    "skipped",
                ));
                continue;
            }

            planned_downloads += 1;
            bytes_to_download += remote_size;
            results.push(sync_entry_result(
                remote_path,
                &local_path,
                remote_size,
                local_hash,
                remote_hash,
                "planned_download",
            ));
            continue;
        }

        planned_downloads += 1;
        bytes_to_download += remote_size;
        results.push(sync_entry_result(
            remote_path,
            &local_path,
            remote_size,
            String::new(),
            remote_hash,
            "planned_download",
        ));
    }

    let mut map = HashMap::new();
    map.insert("dry_run".into(), Value::Bool(true));
    map.insert("files".into(), Value::Number(files as f64));
    map.insert(
        "planned_downloads".into(),
        Value::Number(planned_downloads as f64),
    );
    map.insert("skipped".into(), Value::Number(skipped as f64));
    map.insert(
        "bytes_to_download".into(),
        Value::Number(bytes_to_download as f64),
    );
    map.insert(
        "local_root".into(),
        Value::String(local_root.to_string_lossy().into_owned()),
    );
    map.insert("entries".into(), Value::List(results));
    Ok(Value::Object(map))
}

fn sync_up_files(
    token: &str,
    local_root: &Path,
    remote_folder: &str,
    remote_hashes: &HashMap<String, String>,
    local_files: &[PathBuf],
) -> Result<Value, String> {
    let mut uploaded = 0usize;
    let mut skipped = 0usize;
    let mut bytes_uploaded = 0usize;
    let mut results = Vec::new();

    for local_path in local_files {
        let relative = local_relative_path(local_root, local_path)?;
        let remote_path = remote_path_for_local(remote_folder, &relative)?;
        let bytes = fs::read(local_path)
            .map_err(|e| format!("Failed to read '{}': {}", local_path.display(), e))?;
        let local_hash = dropbox_content_hash(&bytes);

        let remote_key = remote_path.to_lowercase();
        if let Some(remote_hash) = remote_hashes
            .get(&remote_key)
            .filter(|remote_hash| local_hash.eq_ignore_ascii_case(remote_hash))
        {
            skipped += 1;
            results.push(sync_entry_result(
                &remote_path,
                local_path,
                bytes.len(),
                local_hash,
                remote_hash,
                "skipped",
            ));
            continue;
        }

        let metadata = upload_file_bytes(token, &remote_path, UploadMode::overwrite(), &bytes)?;
        let uploaded_hash = metadata
            .get("content_hash")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "Dropbox upload response missing content_hash".to_string())?;
        if !local_hash.eq_ignore_ascii_case(uploaded_hash) {
            return Err(format!(
                "Uploaded '{}' but Dropbox content_hash did not match local file",
                remote_path
            ));
        }

        uploaded += 1;
        bytes_uploaded += bytes.len();
        results.push(sync_entry_result(
            &remote_path,
            local_path,
            bytes.len(),
            local_hash,
            uploaded_hash,
            "uploaded",
        ));
    }

    let mut map = HashMap::new();
    map.insert("files".into(), Value::Number(local_files.len() as f64));
    map.insert("uploaded".into(), Value::Number(uploaded as f64));
    map.insert("skipped".into(), Value::Number(skipped as f64));
    map.insert(
        "bytes_uploaded".into(),
        Value::Number(bytes_uploaded as f64),
    );
    map.insert(
        "local_root".into(),
        Value::String(local_root.to_string_lossy().into_owned()),
    );
    map.insert("remote_root".into(), Value::String(remote_folder.into()));
    map.insert("entries".into(), Value::List(results));
    Ok(Value::Object(map))
}

fn plan_up_files(
    local_root: &Path,
    remote_folder: &str,
    remote_hashes: &HashMap<String, String>,
    local_files: &[PathBuf],
) -> Result<Value, String> {
    let mut planned_uploads = 0usize;
    let mut skipped = 0usize;
    let mut bytes_to_upload = 0usize;
    let mut results = Vec::new();

    for local_path in local_files {
        let relative = local_relative_path(local_root, local_path)?;
        let remote_path = remote_path_for_local(remote_folder, &relative)?;
        let bytes = fs::read(local_path)
            .map_err(|e| format!("Failed to read '{}': {}", local_path.display(), e))?;
        let local_hash = dropbox_content_hash(&bytes);

        let remote_key = remote_path.to_lowercase();
        if let Some(remote_hash) = remote_hashes
            .get(&remote_key)
            .filter(|remote_hash| local_hash.eq_ignore_ascii_case(remote_hash))
        {
            skipped += 1;
            results.push(sync_entry_result(
                &remote_path,
                local_path,
                bytes.len(),
                local_hash,
                remote_hash,
                "skipped",
            ));
            continue;
        }

        planned_uploads += 1;
        bytes_to_upload += bytes.len();
        let remote_hash = remote_hashes
            .get(&remote_key)
            .map(String::as_str)
            .unwrap_or("");
        results.push(sync_entry_result(
            &remote_path,
            local_path,
            bytes.len(),
            local_hash,
            remote_hash,
            "planned_upload",
        ));
    }

    let mut map = HashMap::new();
    map.insert("dry_run".into(), Value::Bool(true));
    map.insert("files".into(), Value::Number(local_files.len() as f64));
    map.insert(
        "planned_uploads".into(),
        Value::Number(planned_uploads as f64),
    );
    map.insert("skipped".into(), Value::Number(skipped as f64));
    map.insert(
        "bytes_to_upload".into(),
        Value::Number(bytes_to_upload as f64),
    );
    map.insert(
        "local_root".into(),
        Value::String(local_root.to_string_lossy().into_owned()),
    );
    map.insert("remote_root".into(), Value::String(remote_folder.into()));
    map.insert("entries".into(), Value::List(results));
    Ok(Value::Object(map))
}

fn collect_local_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    collect_local_files_into(root, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_local_files_into(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in
        fs::read_dir(dir).map_err(|e| format!("Failed to read '{}': {}", dir.display(), e))?
    {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|e| e.to_string())?;

        if file_type.is_dir() {
            collect_local_files_into(&path, files)?;
        } else if file_type.is_file() {
            files.push(path);
        }
    }

    Ok(())
}

fn local_relative_path(root: &Path, path: &Path) -> Result<String, String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| format!("Local path '{}' is outside sync root", path.display()))?;
    let mut parts = Vec::new();

    for component in relative.components() {
        let std::path::Component::Normal(part) = component else {
            return Err(format!("Unsafe local path '{}'", path.display()));
        };
        let part = part.to_string_lossy();
        if part.is_empty() || matches!(part.as_ref(), "." | "..") {
            return Err(format!("Unsafe local path '{}'", path.display()));
        }
        parts.push(part.into_owned());
    }

    if parts.is_empty() {
        return Err(format!(
            "Local path '{}' is not a file path",
            path.display()
        ));
    }

    Ok(parts.join("/"))
}

fn remote_path_for_local(remote_folder: &str, relative: &str) -> Result<String, String> {
    safe_join(Path::new(""), relative)?;
    let folder = remote_folder.trim_matches('/');
    if folder.is_empty() {
        Ok(format!("/{}", relative))
    } else {
        Ok(format!("/{}/{}", folder, relative))
    }
}

fn remote_hashes_by_path(entries: &[serde_json::Value]) -> Result<HashMap<String, String>, String> {
    let mut hashes = HashMap::new();

    for entry in entries {
        if entry.get(".tag").and_then(|value| value.as_str()) != Some("file") {
            continue;
        }
        let path = entry
            .get("path_display")
            .or_else(|| entry.get("path_lower"))
            .and_then(|value| value.as_str())
            .ok_or_else(|| "Dropbox file entry missing path".to_string())?;
        let hash = entry
            .get("content_hash")
            .and_then(|value| value.as_str())
            .ok_or_else(|| format!("Dropbox file '{}' missing content_hash", path))?;
        hashes.insert(path.to_lowercase(), hash.to_string());
    }

    Ok(hashes)
}

fn is_dropbox_path_not_found(error: &str) -> bool {
    error.contains("path/not_found") || error.contains("not_found")
}

fn sync_entry_result(
    remote_path: &str,
    local_path: &Path,
    byte_count: usize,
    content_hash: String,
    remote_hash: &str,
    status: &str,
) -> Value {
    let mut map = HashMap::new();
    map.insert("remote_path".into(), Value::String(remote_path.into()));
    map.insert(
        "path".into(),
        Value::String(local_path.to_string_lossy().into_owned()),
    );
    map.insert("bytes".into(), Value::Number(byte_count as f64));
    map.insert("content_hash".into(), Value::String(content_hash));
    map.insert(
        "remote_content_hash".into(),
        Value::String(remote_hash.into()),
    );
    map.insert("matches".into(), Value::Bool(true));
    map.insert("status".into(), Value::String(status.into()));
    Value::Object(map)
}

fn remote_relative_path(remote_folder: &str, remote_path: &str) -> Result<String, String> {
    let folder = remote_folder.trim_matches('/');
    let path = remote_path.trim_matches('/');

    if folder.is_empty() {
        return Ok(path.to_string());
    }

    path.strip_prefix(folder)
        .map(|relative| relative.trim_start_matches('/').to_string())
        .filter(|relative| !relative.is_empty())
        .ok_or_else(|| {
            format!(
                "Dropbox path '{}' is not inside remote folder '{}'",
                remote_path, remote_folder
            )
        })
}

fn safe_join(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let mut path = root.to_path_buf();
    for part in relative.split('/') {
        if part.is_empty() || matches!(part, "." | "..") || part.contains('\\') {
            return Err(format!("Unsafe Dropbox path component '{}'", part));
        }
        path.push(part);
    }
    Ok(path)
}

fn write_downloaded_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create '{}': {}", parent.display(), e))?;
    }
    fs::write(path, bytes).map_err(|e| format!("Failed to write '{}': {}", path.display(), e))
}

fn dropbox_secrets_import(executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    executor.check_permission("dropbox.read")?;
    executor.check_permission("secrets.write")?;

    let args = arg_strings(executor, call.args.clone())?;
    let remote_path = match args.as_slice() {
        [remote_path] => remote_path,
        _ => return Err("dropbox.secrets.import expects <remote-path>".into()),
    };
    let token = access_token(executor, call.config.clone())?;
    let downloaded = download_file(&token, remote_path)?;
    let text = String::from_utf8(downloaded.bytes)
        .map_err(|_| "dropbox.secrets.import expects a UTF-8 JSON file".to_string())?;
    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("Failed to parse secrets JSON: {}", e))?;
    let secrets = parse_secret_bundle(&json)?;

    for (name, value) in &secrets {
        write_secret(name, value)?;
    }

    let mut map = HashMap::new();
    map.insert("imported".into(), Value::Number(secrets.len() as f64));
    map.insert(
        "names".into(),
        Value::List(
            secrets
                .into_iter()
                .map(|(name, _)| Value::String(name))
                .collect(),
        ),
    );
    Ok(Value::Object(map))
}

fn dropbox_secrets_save(executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    executor.check_permission("secrets.write")?;
    if !call.args.is_empty() {
        return Err("dropbox.secrets.save expects no arguments".into());
    }

    let env = call_env(executor, call.config.clone())?;
    let secrets = dropbox_secrets_from_env(&env)?;

    for (name, value) in &secrets {
        write_secret(name, value)?;
    }

    let mut map = HashMap::new();
    map.insert("saved".into(), Value::Number(secrets.len() as f64));
    map.insert(
        "names".into(),
        Value::List(
            secrets
                .into_iter()
                .map(|(name, _)| Value::String(name))
                .collect(),
        ),
    );
    Ok(Value::Object(map))
}

struct DownloadedFile {
    bytes: Vec<u8>,
    metadata: Option<serde_json::Value>,
}

fn download_file(token: &str, remote_path: &str) -> Result<DownloadedFile, String> {
    let api_arg = json!({ "path": remote_path }).to_string();
    let response = ureq::post("https://content.dropboxapi.com/2/files/download")
        .set("Authorization", &format!("Bearer {}", token))
        .set("Dropbox-API-Arg", &api_arg)
        .call()
        .map_err(describe_ureq_error)?;

    let metadata = response
        .header("Dropbox-API-Result")
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok());
    let mut bytes = Vec::new();
    response
        .into_reader()
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Failed to read Dropbox download: {}", e))?;

    Ok(DownloadedFile { bytes, metadata })
}

fn parse_secret_bundle(json: &serde_json::Value) -> Result<Vec<(String, String)>, String> {
    let object = json
        .get("secrets")
        .unwrap_or(json)
        .as_object()
        .ok_or_else(|| "Secrets bundle must be a JSON object".to_string())?;
    let mut secrets = Vec::new();

    for (name, value) in object {
        let Some(secret) = value.as_str() else {
            return Err(format!("Secret '{}' must be a string value", name));
        };
        if name.contains('/') || name.contains('\\') || name.split('.').any(str::is_empty) {
            return Err(format!("Invalid secret name '{}'", name));
        }
        secrets.push((name.clone(), secret.to_string()));
    }

    secrets.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(secrets)
}

fn dropbox_upload(executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    executor.check_permission("dropbox.write")?;

    let args = arg_strings(executor, call.args.clone())?;
    let (local_path, remote_path, upload_mode) = upload_request_parts(
        &args,
        "dropbox.upload expects <local-path> <remote-path> [add|rename|overwrite]",
    )?;
    let token = access_token(executor, call.config.clone())?;
    let bytes =
        fs::read(local_path).map_err(|e| format!("Failed to read '{}': {}", local_path, e))?;
    let json = upload_file_bytes(&token, remote_path, upload_mode, &bytes)?;

    Ok(json_to_value(json))
}

fn dropbox_upload_verify(executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    executor.check_permission("dropbox.write")?;

    let args = arg_strings(executor, call.args.clone())?;
    let (local_path, remote_path, upload_mode) = upload_request_parts(
        &args,
        "dropbox.upload.verify expects <local-path> <remote-path> [add|rename|overwrite]",
    )?;
    let token = access_token(executor, call.config.clone())?;
    let bytes =
        fs::read(local_path).map_err(|e| format!("Failed to read '{}': {}", local_path, e))?;
    let local_hash = dropbox_content_hash(&bytes);
    let metadata = upload_file_bytes(&token, remote_path, upload_mode, &bytes)?;

    upload_verification_result(local_path, remote_path, bytes.len(), local_hash, metadata)
}

fn upload_request_parts<'a>(
    args: &'a [String],
    usage_error: &str,
) -> Result<(&'a String, &'a String, UploadMode), String> {
    match args {
        [local_path, remote_path] => Ok((local_path, remote_path, UploadMode::safe_add())),
        [local_path, remote_path, mode] => Ok((local_path, remote_path, UploadMode::parse(mode)?)),
        _ => Err(usage_error.into()),
    }
}

fn upload_file_bytes(
    token: &str,
    remote_path: &str,
    upload_mode: UploadMode,
    bytes: &[u8],
) -> Result<serde_json::Value, String> {
    let api_arg = json!({
        "path": remote_path,
        "mode": upload_mode.write_mode,
        "autorename": upload_mode.autorename,
        "mute": false,
        "strict_conflict": upload_mode.strict_conflict
    })
    .to_string();
    let response = ureq::post("https://content.dropboxapi.com/2/files/upload")
        .set("Authorization", &format!("Bearer {}", token))
        .set("Content-Type", "application/octet-stream")
        .set("Dropbox-API-Arg", &api_arg)
        .send_bytes(bytes)
        .map_err(describe_ureq_error)?;

    response_json(response)
}

fn upload_verification_result(
    local_path: &str,
    remote_path: &str,
    byte_count: usize,
    local_hash: String,
    metadata: serde_json::Value,
) -> Result<Value, String> {
    let uploaded_hash = metadata
        .get("content_hash")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "Dropbox upload response missing content_hash".to_string())?
        .to_string();
    let matches = local_hash.eq_ignore_ascii_case(&uploaded_hash);

    let mut map = HashMap::new();
    map.insert("path".into(), Value::String(local_path.into()));
    map.insert("remote_path".into(), Value::String(remote_path.into()));
    map.insert("bytes".into(), Value::Number(byte_count as f64));
    map.insert("content_hash".into(), Value::String(local_hash));
    map.insert("uploaded_content_hash".into(), Value::String(uploaded_hash));
    map.insert("matches".into(), Value::Bool(matches));
    map.insert("metadata".into(), json_to_value(metadata));
    Ok(Value::Object(map))
}

fn dropbox_hash(executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    let args = arg_strings(executor, call.args.clone())?;
    let local_path = match args.as_slice() {
        [local_path] => local_path,
        _ => return Err("dropbox.hash expects <local-path>".into()),
    };
    let bytes =
        fs::read(local_path).map_err(|e| format!("Failed to read '{}': {}", local_path, e))?;
    let content_hash = dropbox_content_hash(&bytes);

    let mut map = HashMap::new();
    map.insert("path".into(), Value::String(local_path.clone()));
    map.insert("bytes".into(), Value::Number(bytes.len() as f64));
    map.insert("content_hash".into(), Value::String(content_hash));
    Ok(Value::Object(map))
}

fn dropbox_verify(executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    let args = arg_strings(executor, call.args.clone())?;
    let (local_path, expected_hash) = match args.as_slice() {
        [local_path, expected_hash] => (local_path, expected_hash),
        _ => return Err("dropbox.verify expects <local-path> <content-hash>".into()),
    };
    let bytes =
        fs::read(local_path).map_err(|e| format!("Failed to read '{}': {}", local_path, e))?;
    let content_hash = dropbox_content_hash(&bytes);
    let matches = content_hash.eq_ignore_ascii_case(expected_hash);

    let mut map = HashMap::new();
    map.insert("path".into(), Value::String(local_path.clone()));
    map.insert("bytes".into(), Value::Number(bytes.len() as f64));
    map.insert("content_hash".into(), Value::String(content_hash));
    map.insert("expected_hash".into(), Value::String(expected_hash.clone()));
    map.insert("matches".into(), Value::Bool(matches));
    Ok(Value::Object(map))
}

fn dropbox_content_hash(bytes: &[u8]) -> String {
    let mut block_hashes = Vec::new();

    for block in bytes.chunks(DROPBOX_CONTENT_HASH_BLOCK_SIZE) {
        block_hashes.extend_from_slice(digest(&SHA256, block).as_ref());
    }

    hex_digest(digest(&SHA256, &block_hashes).as_ref())
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push_str(&format!("{:02x}", byte));
    }
    output
}

#[derive(Debug)]
struct UploadMode {
    write_mode: &'static str,
    autorename: bool,
    strict_conflict: bool,
}

impl UploadMode {
    fn safe_add() -> Self {
        Self {
            write_mode: "add",
            autorename: false,
            strict_conflict: true,
        }
    }

    fn parse(mode: &str) -> Result<Self, String> {
        match mode {
            "add" | "safe" => Ok(Self::safe_add()),
            "rename" => Ok(Self {
                write_mode: "add",
                autorename: true,
                strict_conflict: true,
            }),
            "overwrite" => Ok(Self {
                write_mode: "overwrite",
                autorename: false,
                strict_conflict: false,
            }),
            _ => Err("dropbox.upload mode must be 'add', 'rename', or 'overwrite'".into()),
        }
    }

    fn overwrite() -> Self {
        Self {
            write_mode: "overwrite",
            autorename: false,
            strict_conflict: false,
        }
    }
}

fn dropbox_delete(executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    executor.check_permission("dropbox.write")?;

    let args = arg_strings(executor, call.args.clone())?;
    let path = match args.as_slice() {
        [path] => path,
        _ => return Err("dropbox.delete expects <remote-path>".into()),
    };
    let token = access_token(executor, call.config.clone())?;
    let body = json!({ "path": path });
    let response = ureq::post("https://api.dropboxapi.com/2/files/delete_v2")
        .set("Authorization", &format!("Bearer {}", token))
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())
        .map_err(describe_ureq_error)?;
    let json = response_json(response)?;

    Ok(json_to_value(json))
}

fn arg_strings(executor: &mut dyn PluginHost, args: Vec<Expr>) -> Result<Vec<String>, String> {
    args.into_iter()
        .map(|expr| executor.plugin_arg_value(expr).map(value_to_string))
        .collect()
}

fn access_token(executor: &mut dyn PluginHost, config: Option<CallConfig>) -> Result<String, String> {
    let env = call_env(executor, config)?;
    access_token_from_env(env)
}

fn access_token_from_env(env: HashMap<String, String>) -> Result<String, String> {
    if let Some(token) = configured_or_process_env(&env, "DROPBOX_TOKEN")
        .or_else(|| secret_value("dropbox.access_token"))
    {
        return Ok(token);
    }

    let refresh_token = configured_or_process_env(&env, "DROPBOX_REFRESH_TOKEN")
        .or_else(|| secret_value("dropbox.refresh_token"))
        .ok_or_else(|| {
        "Dropbox commands need DROPBOX_TOKEN or DROPBOX_REFRESH_TOKEN in the environment or call config"
            .to_string()
    })?;
    let configured_app_key = app_key_from_env(&env);
    let app_key = configured_app_key
        .clone()
        .or_else(|| secret_value("dropbox.app_key"))
        .ok_or_else(|| {
            "Dropbox refresh-token auth needs DROPBOX_APP_KEY or DROPBOX_CLIENT_ID".to_string()
        })?;
    let app_secret = app_secret_from_env_or_secret(&env, configured_app_key.is_none());

    refresh_access_token(&refresh_token, &app_key, app_secret.as_deref())
}

struct CredentialSummary {
    configured: bool,
    source: &'static str,
}

fn credential_summary(env: &HashMap<String, String>) -> CredentialSummary {
    if configured_or_process_env(env, "DROPBOX_TOKEN")
        .or_else(|| secret_value("dropbox.access_token"))
        .is_some()
    {
        return CredentialSummary {
            configured: true,
            source: "access_token",
        };
    }

    let has_refresh = configured_or_process_env(env, "DROPBOX_REFRESH_TOKEN")
        .or_else(|| secret_value("dropbox.refresh_token"))
        .is_some();
    let has_app_key = configured_or_process_env(env, "DROPBOX_APP_KEY")
        .or_else(|| configured_or_process_env(env, "DROPBOX_CLIENT_ID"))
        .or_else(|| secret_value("dropbox.app_key"))
        .is_some();

    CredentialSummary {
        configured: has_refresh && has_app_key,
        source: if has_refresh && has_app_key {
            "refresh_token"
        } else {
            "none"
        },
    }
}

fn call_env(
    executor: &mut dyn PluginHost,
    config: Option<CallConfig>,
) -> Result<HashMap<String, String>, String> {
    let mut env = HashMap::new();

    if let Some(config) = config {
        for (key, expr) in config.env {
            env.insert(key, value_to_string(executor.plugin_arg_value(expr)?));
        }
    }

    Ok(env)
}

fn configured_or_process_env(env: &HashMap<String, String>, key: &str) -> Option<String> {
    env.get(key)
        .cloned()
        .or_else(|| std::env::var(key).ok())
        .filter(|value| !value.is_empty())
}

fn app_key_from_env(env: &HashMap<String, String>) -> Option<String> {
    configured_or_process_env(env, "DROPBOX_APP_KEY")
        .or_else(|| configured_or_process_env(env, "DROPBOX_CLIENT_ID"))
}

fn app_key_from_env_or_secret(env: &HashMap<String, String>) -> Option<String> {
    app_key_from_env(env).or_else(|| secret_value("dropbox.app_key"))
}

fn app_secret_from_env_or_secret(
    env: &HashMap<String, String>,
    allow_saved_secret: bool,
) -> Option<String> {
    configured_or_process_env(env, "DROPBOX_APP_SECRET")
        .or_else(|| configured_or_process_env(env, "DROPBOX_CLIENT_SECRET"))
        .or_else(|| {
            allow_saved_secret
                .then(|| secret_value("dropbox.app_secret"))
                .flatten()
        })
}

fn dropbox_secrets_from_env(
    env: &HashMap<String, String>,
) -> Result<Vec<(String, String)>, String> {
    if let Some(token) = configured_or_process_env(env, "DROPBOX_TOKEN") {
        return Ok(vec![("dropbox.access_token".into(), token)]);
    }

    let refresh_token = configured_or_process_env(env, "DROPBOX_REFRESH_TOKEN")
        .ok_or_else(|| {
            "dropbox.secrets.save needs DROPBOX_TOKEN or DROPBOX_REFRESH_TOKEN in env config or process environment"
                .to_string()
        })?;
    let app_key = configured_or_process_env(env, "DROPBOX_APP_KEY")
        .or_else(|| configured_or_process_env(env, "DROPBOX_CLIENT_ID"))
        .ok_or_else(|| {
            "dropbox.secrets.save refresh-token auth needs DROPBOX_APP_KEY or DROPBOX_CLIENT_ID"
                .to_string()
        })?;
    let mut secrets = vec![
        ("dropbox.refresh_token".into(), refresh_token),
        ("dropbox.app_key".into(), app_key),
    ];

    if let Some(app_secret) = configured_or_process_env(env, "DROPBOX_APP_SECRET")
        .or_else(|| configured_or_process_env(env, "DROPBOX_CLIENT_SECRET"))
    {
        secrets.push(("dropbox.app_secret".into(), app_secret));
    }

    Ok(secrets)
}

fn dropbox_oauth_url(app_key: &str) -> String {
    format!(
        "https://www.dropbox.com/oauth2/authorize?client_id={}&response_type=code&token_access_type=offline",
        url_encode_component(app_key)
    )
}

fn finish_oauth_code(
    code: &str,
    app_key: &str,
    app_secret: Option<&str>,
) -> Result<serde_json::Value, String> {
    let mut form = vec![("grant_type", "authorization_code"), ("code", code)];
    if app_secret.is_none() {
        form.push(("client_id", app_key));
    }

    oauth_token_json(&form, app_key, app_secret, false)
}

fn oauth_secrets(
    token: &serde_json::Value,
    app_key: &str,
    app_secret: Option<&str>,
) -> Result<Vec<(String, String)>, String> {
    let mut secrets = vec![("dropbox.app_key".into(), app_key.to_string())];

    if let Some(app_secret) = app_secret {
        secrets.push(("dropbox.app_secret".into(), app_secret.to_string()));
    }

    if let Some(refresh_token) = token.get("refresh_token").and_then(|value| value.as_str()) {
        secrets.insert(
            0,
            ("dropbox.refresh_token".into(), refresh_token.to_string()),
        );
    } else if let Some(access_token) = token.get("access_token").and_then(|value| value.as_str()) {
        secrets.insert(0, ("dropbox.access_token".into(), access_token.to_string()));
    } else {
        return Err("Dropbox OAuth response did not include refresh_token or access_token".into());
    }

    Ok(secrets)
}

fn secret_value(name: &str) -> Option<String> {
    read_secret(name).ok().flatten()
}

fn url_encode_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn refresh_access_token(
    refresh_token: &str,
    app_key: &str,
    app_secret: Option<&str>,
) -> Result<String, String> {
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
    ];
    if app_secret.is_none() {
        form.push(("client_id", app_key));
    }

    let json = oauth_token_json(&form, app_key, app_secret, true)?;

    json.get("access_token")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .ok_or_else(|| "Dropbox refresh-token response did not include access_token".into())
}

fn oauth_token_json(
    form: &[(&str, &str)],
    app_key: &str,
    app_secret: Option<&str>,
    retry_secret_in_form: bool,
) -> Result<serde_json::Value, String> {
    let request = ureq::post("https://api.dropboxapi.com/oauth2/token");
    let request = if let Some(app_secret) = app_secret {
        request.set(
            "Authorization",
            &format!("Basic {}", basic_auth_token(app_key, app_secret)),
        )
    } else {
        request
    };
    match request.send_form(form) {
        Ok(response) => response_json(response),
        Err(error) if retry_secret_in_form && app_secret.is_some() => {
            let first_error = describe_ureq_error(error);
            let app_secret = app_secret.expect("checked above");
            let mut fallback = form.to_vec();
            fallback.push(("client_id", app_key));
            fallback.push(("client_secret", app_secret));

            let response = ureq::post("https://api.dropboxapi.com/oauth2/token")
                .send_form(&fallback)
                .map_err(|error| {
                    format!(
                        "{}; retry with client_secret form field failed: {}",
                        first_error,
                        describe_ureq_error(error)
                    )
                })?;
            response_json(response)
        }
        Err(error) => Err(describe_ureq_error(error)),
    }
}

fn basic_auth_token(username: &str, password: &str) -> String {
    base64_encode(
        format!(
            "{}:{}",
            url_encode_component(username),
            url_encode_component(password)
        )
        .as_bytes(),
    )
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::new();

    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let value = ((b0 as u32) << 16) | ((b1 as u32) << 8) | b2 as u32;

        encoded.push(TABLE[((value >> 18) & 0x3f) as usize] as char);
        encoded.push(TABLE[((value >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            encoded.push(TABLE[((value >> 6) & 0x3f) as usize] as char);
        } else {
            encoded.push('=');
        }
        if chunk.len() > 2 {
            encoded.push(TABLE[(value & 0x3f) as usize] as char);
        } else {
            encoded.push('=');
        }
    }

    encoded
}

fn account_json(token: &str) -> Result<serde_json::Value, String> {
    let response = ureq::post("https://api.dropboxapi.com/2/users/get_current_account")
        .set("Authorization", &format!("Bearer {}", token))
        .call()
        .map_err(describe_ureq_error)?;
    response_json(response)
}

fn value_to_string(value: Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) | Value::Secret(value) => value,
        other => format!("{:?}", other),
    }
}

fn response_json(response: ureq::Response) -> Result<serde_json::Value, String> {
    let text = response
        .into_string()
        .map_err(|e| format!("Failed to read Dropbox response: {}", e))?;
    serde_json::from_str(&text).map_err(|e| format!("Failed to parse Dropbox response: {}", e))
}

fn describe_ureq_error(error: ureq::Error) -> String {
    match error {
        ureq::Error::Status(status, response) => {
            let body = response.into_string().unwrap_or_default();
            if body.is_empty() {
                format!("Dropbox request failed with status {}", status)
            } else {
                format!("Dropbox request failed with status {}: {}", status, body)
            }
        }
        ureq::Error::Transport(error) => format!("Dropbox request failed: {}", error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::PermissionSet;

    fn assert_bool_field(map: &HashMap<String, Value>, key: &str, expected: bool) {
        match map.get(key) {
            Some(Value::Bool(actual)) => assert_eq!(*actual, expected),
            other => panic!("Expected bool field '{}', got {:?}", key, other),
        }
    }

    fn assert_number_field(map: &HashMap<String, Value>, key: &str, expected: f64) {
        match map.get(key) {
            Some(Value::Number(actual)) => assert_eq!(*actual, expected),
            other => panic!("Expected number field '{}', got {:?}", key, other),
        }
    }

    #[test]
    fn dropbox_plugin_ignores_unknown_calls() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = DropboxPlugin
            .call(
                &mut executor,
                &FunctionCall {
                    name: vec!["not_dropbox".into()],
                    args: Vec::new(),
                    config: None,
                },
                &Value::Null,
            )
            .unwrap();

        assert!(!result.is_handled());
    }

    #[test]
    fn dropbox_list_requires_dropbox_read_permission() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = DropboxPlugin.call(
            &mut executor,
            &FunctionCall {
                name: vec!["dropbox".into(), "list".into()],
                args: vec![Expr::String("".into())],
                config: None,
            },
            &Value::Null,
        );

        match result {
            Err(err) => assert!(err.contains("dropbox.read")),
            Ok(_) => panic!("Expected dropbox.list to require dropbox.read"),
        }
    }

    #[test]
    fn dropbox_status_requires_dropbox_read_permission() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = DropboxPlugin.call(
            &mut executor,
            &FunctionCall {
                name: vec!["dropbox".into(), "status".into()],
                args: Vec::new(),
                config: None,
            },
            &Value::Null,
        );

        match result {
            Err(err) => assert!(err.contains("dropbox.read")),
            Ok(_) => panic!("Expected dropbox.status to require dropbox.read"),
        }
    }

    #[test]
    fn dropbox_auth_finish_requires_secrets_write_permission() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = DropboxPlugin.call(
            &mut executor,
            &FunctionCall {
                name: vec!["dropbox".into(), "auth".into(), "finish".into()],
                args: vec![Expr::String("code".into())],
                config: None,
            },
            &Value::Null,
        );

        match result {
            Err(err) => assert!(err.contains("secrets.write")),
            Ok(_) => panic!("Expected dropbox.auth.finish to require secrets.write"),
        }
    }

    #[test]
    fn dropbox_account_requires_dropbox_read_permission() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = DropboxPlugin.call(
            &mut executor,
            &FunctionCall {
                name: vec!["dropbox".into(), "account".into()],
                args: Vec::new(),
                config: None,
            },
            &Value::Null,
        );

        match result {
            Err(err) => assert!(err.contains("dropbox.read")),
            Ok(_) => panic!("Expected dropbox.account to require dropbox.read"),
        }
    }

    #[test]
    fn dropbox_metadata_requires_dropbox_read_permission() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = DropboxPlugin.call(
            &mut executor,
            &FunctionCall {
                name: vec!["dropbox".into(), "metadata".into()],
                args: vec![Expr::String("/reports/q1.csv".into())],
                config: None,
            },
            &Value::Null,
        );

        match result {
            Err(err) => assert!(err.contains("dropbox.read")),
            Ok(_) => panic!("Expected dropbox.metadata to require dropbox.read"),
        }
    }

    #[test]
    fn dropbox_download_verify_requires_dropbox_read_permission() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = DropboxPlugin.call(
            &mut executor,
            &FunctionCall {
                name: vec!["dropbox".into(), "download".into(), "verify".into()],
                args: vec![
                    Expr::String("/reports/q1.csv".into()),
                    Expr::String("q1.csv".into()),
                ],
                config: None,
            },
            &Value::Null,
        );

        match result {
            Err(err) => assert!(err.contains("dropbox.read")),
            Ok(_) => panic!("Expected dropbox.download.verify to require dropbox.read"),
        }
    }

    #[test]
    fn dropbox_sync_down_requires_dropbox_read_permission() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = DropboxPlugin.call(
            &mut executor,
            &FunctionCall {
                name: vec!["dropbox".into(), "sync".into(), "down".into()],
                args: vec![
                    Expr::String("/reports".into()),
                    Expr::String("reports".into()),
                ],
                config: None,
            },
            &Value::Null,
        );

        match result {
            Err(err) => assert!(err.contains("dropbox.read")),
            Ok(_) => panic!("Expected dropbox.sync.down to require dropbox.read"),
        }
    }

    #[test]
    fn dropbox_sync_plan_down_requires_dropbox_read_permission() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = DropboxPlugin.call(
            &mut executor,
            &FunctionCall {
                name: vec![
                    "dropbox".into(),
                    "sync".into(),
                    "plan".into(),
                    "down".into(),
                ],
                args: vec![
                    Expr::String("/reports".into()),
                    Expr::String("reports".into()),
                ],
                config: None,
            },
            &Value::Null,
        );

        match result {
            Err(err) => assert!(err.contains("dropbox.read")),
            Ok(_) => panic!("Expected dropbox.sync.plan.down to require dropbox.read"),
        }
    }

    #[test]
    fn dropbox_sync_up_requires_dropbox_read_permission_first() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = DropboxPlugin.call(
            &mut executor,
            &FunctionCall {
                name: vec!["dropbox".into(), "sync".into(), "up".into()],
                args: vec![
                    Expr::String("reports".into()),
                    Expr::String("/reports".into()),
                ],
                config: None,
            },
            &Value::Null,
        );

        match result {
            Err(err) => assert!(err.contains("dropbox.read")),
            Ok(_) => panic!("Expected dropbox.sync.up to require dropbox.read"),
        }
    }

    #[test]
    fn dropbox_sync_up_requires_dropbox_write_permission_after_read() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&vec![(
            "dropbox".into(),
            "read".into(),
        )]));
        let result = DropboxPlugin.call(
            &mut executor,
            &FunctionCall {
                name: vec!["dropbox".into(), "sync".into(), "up".into()],
                args: vec![
                    Expr::String("reports".into()),
                    Expr::String("/reports".into()),
                ],
                config: None,
            },
            &Value::Null,
        );

        match result {
            Err(err) => assert!(err.contains("dropbox.write")),
            Ok(_) => panic!("Expected dropbox.sync.up to require dropbox.write"),
        }
    }

    #[test]
    fn dropbox_sync_plan_up_requires_dropbox_read_permission_only() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = DropboxPlugin.call(
            &mut executor,
            &FunctionCall {
                name: vec!["dropbox".into(), "sync".into(), "plan".into(), "up".into()],
                args: vec![
                    Expr::String("reports".into()),
                    Expr::String("/reports".into()),
                ],
                config: None,
            },
            &Value::Null,
        );

        match result {
            Err(err) => assert!(err.contains("dropbox.read")),
            Ok(_) => panic!("Expected dropbox.sync.plan.up to require dropbox.read"),
        }
    }

    #[test]
    fn dropbox_upload_requires_dropbox_write_permission() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = DropboxPlugin.call(
            &mut executor,
            &FunctionCall {
                name: vec!["dropbox".into(), "upload".into()],
                args: vec![
                    Expr::String("local.txt".into()),
                    Expr::String("/remote.txt".into()),
                ],
                config: None,
            },
            &Value::Null,
        );

        match result {
            Err(err) => assert!(err.contains("dropbox.write")),
            Ok(_) => panic!("Expected dropbox.upload to require dropbox.write"),
        }
    }

    #[test]
    fn dropbox_upload_verify_requires_dropbox_write_permission() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = DropboxPlugin.call(
            &mut executor,
            &FunctionCall {
                name: vec!["dropbox".into(), "upload".into(), "verify".into()],
                args: vec![
                    Expr::String("local.txt".into()),
                    Expr::String("/remote.txt".into()),
                ],
                config: None,
            },
            &Value::Null,
        );

        match result {
            Err(err) => assert!(err.contains("dropbox.write")),
            Ok(_) => panic!("Expected dropbox.upload.verify to require dropbox.write"),
        }
    }

    #[test]
    fn dropbox_content_hash_hashes_empty_file_like_sha256_empty() {
        assert_eq!(
            dropbox_content_hash(&[]),
            "e3b0c44298fc1c149afbf4c8996fb924\
             27ae41e4649b934ca495991b7852b855"
                .replace(' ', "")
        );
    }

    #[test]
    fn dropbox_content_hash_hashes_single_block_as_hash_of_block_hash() {
        assert_eq!(
            dropbox_content_hash(b"a"),
            "bf5d3affb73efd2ec6c36ad3112dd933\
             efed63c4e1cbffcfa88e2759c144f2d8"
                .replace(' ', "")
        );
    }

    #[test]
    fn dropbox_content_hash_hashes_multiple_blocks() {
        let mut bytes = vec![b'a'; DROPBOX_CONTENT_HASH_BLOCK_SIZE];
        bytes.push(b'b');

        assert_eq!(
            dropbox_content_hash(&bytes),
            "565546ad93383e225e7cf808fb4d527a\
             54dec54826a5c34a24c1f19a03c62583"
                .replace(' ', "")
        );
    }

    #[test]
    fn dropbox_hash_reports_local_file_content_hash() {
        let path = std::env::temp_dir().join(format!(
            "zen-dropbox-hash-{}-{}.txt",
            std::process::id(),
            "single"
        ));
        fs::write(&path, b"a").unwrap();

        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let value = DropboxPlugin
            .call(
                &mut executor,
                &FunctionCall {
                    name: vec!["dropbox".into(), "hash".into()],
                    args: vec![Expr::String(path.to_string_lossy().into_owned())],
                    config: None,
                },
                &Value::Null,
            )
            .unwrap();
        let _ = fs::remove_file(&path);

        let PluginResult::Handled(Value::Object(map)) = value else {
            panic!("Expected dropbox.hash object");
        };

        assert_eq!(
            map.get("content_hash").and_then(Value::as_string),
            Some("bf5d3affb73efd2ec6c36ad3112dd933efed63c4e1cbffcfa88e2759c144f2d8")
        );
    }

    #[test]
    fn dropbox_verify_compares_local_file_content_hash() {
        let path = std::env::temp_dir().join(format!(
            "zen-dropbox-hash-{}-{}.txt",
            std::process::id(),
            "verify"
        ));
        fs::write(&path, b"a").unwrap();

        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let value = DropboxPlugin
            .call(
                &mut executor,
                &FunctionCall {
                    name: vec!["dropbox".into(), "verify".into()],
                    args: vec![
                        Expr::String(path.to_string_lossy().into_owned()),
                        Expr::String(
                            "bf5d3affb73efd2ec6c36ad3112dd933efed63c4e1cbffcfa88e2759c144f2d8"
                                .into(),
                        ),
                    ],
                    config: None,
                },
                &Value::Null,
            )
            .unwrap();
        let _ = fs::remove_file(&path);

        let PluginResult::Handled(Value::Object(map)) = value else {
            panic!("Expected dropbox.verify object");
        };

        assert!(matches!(map.get("matches"), Some(Value::Bool(true))));
    }

    #[test]
    fn upload_verification_result_compares_uploaded_content_hash() {
        let metadata = serde_json::json!({
            "name": "q1.csv",
            "path_display": "/reports/q1.csv",
            "content_hash": "bf5d3affb73efd2ec6c36ad3112dd933efed63c4e1cbffcfa88e2759c144f2d8"
        });

        let value = upload_verification_result(
            "q1.csv",
            "/reports/q1.csv",
            1,
            "bf5d3affb73efd2ec6c36ad3112dd933efed63c4e1cbffcfa88e2759c144f2d8".into(),
            metadata,
        )
        .unwrap();

        let Value::Object(map) = value else {
            panic!("Expected upload verification object");
        };

        assert!(matches!(map.get("matches"), Some(Value::Bool(true))));
        assert_eq!(
            map.get("uploaded_content_hash").and_then(Value::as_string),
            Some("bf5d3affb73efd2ec6c36ad3112dd933efed63c4e1cbffcfa88e2759c144f2d8")
        );
        assert!(matches!(map.get("metadata"), Some(Value::Object(_))));
    }

    #[test]
    fn download_verification_result_compares_downloaded_content_hash() {
        let metadata = serde_json::json!({
            "name": "q1.csv",
            "path_display": "/reports/q1.csv",
            "content_hash": "bf5d3affb73efd2ec6c36ad3112dd933efed63c4e1cbffcfa88e2759c144f2d8"
        });

        let value = download_verification_result(
            "/reports/q1.csv",
            "q1.csv",
            1,
            "bf5d3affb73efd2ec6c36ad3112dd933efed63c4e1cbffcfa88e2759c144f2d8".into(),
            Some(metadata),
        )
        .unwrap();

        let Value::Object(map) = value else {
            panic!("Expected download verification object");
        };

        assert!(matches!(map.get("matches"), Some(Value::Bool(true))));
        assert_eq!(
            map.get("remote_content_hash").and_then(Value::as_string),
            Some("bf5d3affb73efd2ec6c36ad3112dd933efed63c4e1cbffcfa88e2759c144f2d8")
        );
        assert!(matches!(map.get("metadata"), Some(Value::Object(_))));
    }

    #[test]
    fn remote_relative_path_maps_nested_dropbox_paths() {
        assert_eq!(
            remote_relative_path("/reports", "/reports/2026/q1.csv").unwrap(),
            "2026/q1.csv"
        );
        assert_eq!(
            remote_relative_path("", "/reports/2026/q1.csv").unwrap(),
            "reports/2026/q1.csv"
        );
        assert!(remote_relative_path("/reports", "/other/q1.csv").is_err());
    }

    #[test]
    fn safe_join_rejects_unsafe_remote_components() {
        let root = PathBuf::from("downloads");

        assert!(safe_join(&root, "reports/q1.csv").is_ok());
        assert!(safe_join(&root, "../q1.csv").is_err());
        assert!(safe_join(&root, "reports\\q1.csv").is_err());
        assert!(safe_join(&root, "reports//q1.csv").is_err());
    }

    #[test]
    fn remote_path_for_local_maps_relative_paths() {
        assert_eq!(
            remote_path_for_local("/reports", "2026/q1.csv").unwrap(),
            "/reports/2026/q1.csv"
        );
        assert_eq!(
            remote_path_for_local("reports", "2026/q1.csv").unwrap(),
            "/reports/2026/q1.csv"
        );
        assert_eq!(
            remote_path_for_local("", "2026/q1.csv").unwrap(),
            "/2026/q1.csv"
        );
        assert!(remote_path_for_local("/reports", "../q1.csv").is_err());
    }

    #[test]
    fn remote_hashes_by_path_indexes_file_entries_case_insensitively() {
        let entries = vec![
            serde_json::json!({
                ".tag": "file",
                "path_display": "/Reports/Q1.csv",
                "content_hash": "abc"
            }),
            serde_json::json!({
                ".tag": "folder",
                "path_display": "/Reports/Archive"
            }),
        ];
        let hashes = remote_hashes_by_path(&entries).unwrap();

        assert_eq!(
            hashes.get("/reports/q1.csv").map(String::as_str),
            Some("abc")
        );
        assert!(!hashes.contains_key("/reports/archive"));
    }

    #[test]
    fn plan_down_entries_reports_skips_and_downloads_without_writing() {
        let root = std::env::temp_dir().join(format!("zen-plan-down-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let local_bytes = b"same";
        fs::write(root.join("same.txt"), local_bytes).unwrap();
        let local_hash = dropbox_content_hash(local_bytes);
        let entries = vec![
            json!({
                ".tag": "file",
                "path_display": "/reports/same.txt",
                "content_hash": local_hash,
                "size": local_bytes.len()
            }),
            json!({
                ".tag": "file",
                "path_display": "/reports/new.txt",
                "content_hash": dropbox_content_hash(b"new"),
                "size": 3
            }),
        ];

        let result = plan_down_entries("/reports", &root, &entries).unwrap();
        let Value::Object(map) = result else {
            panic!("Expected object");
        };

        assert_bool_field(&map, "dry_run", true);
        assert_number_field(&map, "planned_downloads", 1.0);
        assert_number_field(&map, "skipped", 1.0);
        assert!(!root.join("new.txt").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn plan_up_files_reports_skips_and_uploads_without_uploading() {
        let root = std::env::temp_dir().join(format!("zen-plan-up-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let same_bytes = b"same";
        let changed_bytes = b"changed";
        fs::write(root.join("same.txt"), same_bytes).unwrap();
        fs::write(root.join("changed.txt"), changed_bytes).unwrap();
        let mut remote_hashes = HashMap::new();
        remote_hashes.insert("/reports/same.txt".into(), dropbox_content_hash(same_bytes));

        let local_files = collect_local_files(&root).unwrap();
        let result = plan_up_files(&root, "/reports", &remote_hashes, &local_files).unwrap();
        let Value::Object(map) = result else {
            panic!("Expected object");
        };

        assert_bool_field(&map, "dry_run", true);
        assert_number_field(&map, "planned_uploads", 1.0);
        assert_number_field(&map, "skipped", 1.0);
        assert_number_field(&map, "bytes_to_upload", changed_bytes.len() as f64);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dropbox_secrets_import_requires_dropbox_read_permission_first() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = DropboxPlugin.call(
            &mut executor,
            &FunctionCall {
                name: vec!["dropbox".into(), "secrets".into(), "import".into()],
                args: vec![Expr::String("/zen/secrets.json".into())],
                config: None,
            },
            &Value::Null,
        );

        match result {
            Err(err) => assert!(err.contains("dropbox.read")),
            Ok(_) => panic!("Expected dropbox.secrets.import to require dropbox.read"),
        }
    }

    #[test]
    fn dropbox_secrets_save_requires_secrets_write_permission() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = DropboxPlugin.call(
            &mut executor,
            &FunctionCall {
                name: vec!["dropbox".into(), "secrets".into(), "save".into()],
                args: Vec::new(),
                config: None,
            },
            &Value::Null,
        );

        match result {
            Err(err) => assert!(err.contains("secrets.write")),
            Ok(_) => panic!("Expected dropbox.secrets.save to require secrets.write"),
        }
    }

    #[test]
    fn parse_secret_bundle_accepts_wrapped_object() {
        let json = json!({
            "secrets": {
                "dropbox.refresh_token": "refresh",
                "dropbox.app_key": "key"
            }
        });

        let secrets = parse_secret_bundle(&json).unwrap();

        assert_eq!(
            secrets,
            vec![
                ("dropbox.app_key".into(), "key".into()),
                ("dropbox.refresh_token".into(), "refresh".into()),
            ]
        );
    }

    #[test]
    fn parse_secret_bundle_accepts_flat_object() {
        let json = json!({
            "dropbox.refresh_token": "refresh"
        });

        let secrets = parse_secret_bundle(&json).unwrap();

        assert_eq!(
            secrets,
            vec![("dropbox.refresh_token".into(), "refresh".into())]
        );
    }

    #[test]
    fn parse_secret_bundle_rejects_non_string_values() {
        let json = json!({
            "dropbox.refresh_token": 123
        });

        let err = parse_secret_bundle(&json).unwrap_err();

        assert!(err.contains("string"));
    }

    #[test]
    fn parse_secret_bundle_rejects_bad_names() {
        let json = json!({
            "dropbox/refresh_token": "refresh"
        });

        let err = parse_secret_bundle(&json).unwrap_err();

        assert!(err.contains("Invalid secret name"));
    }

    #[test]
    fn dropbox_secrets_from_env_saves_access_token() {
        let mut env = HashMap::new();
        env.insert("DROPBOX_TOKEN".into(), "token".into());

        let secrets = dropbox_secrets_from_env(&env).unwrap();

        assert_eq!(
            secrets,
            vec![("dropbox.access_token".into(), "token".into())]
        );
    }

    #[test]
    fn dropbox_secrets_from_env_saves_refresh_credentials() {
        let mut env = HashMap::new();
        env.insert("DROPBOX_REFRESH_TOKEN".into(), "refresh".into());
        env.insert("DROPBOX_CLIENT_ID".into(), "app-key".into());
        env.insert("DROPBOX_CLIENT_SECRET".into(), "app-secret".into());

        let secrets = dropbox_secrets_from_env(&env).unwrap();

        assert_eq!(
            secrets,
            vec![
                ("dropbox.refresh_token".into(), "refresh".into()),
                ("dropbox.app_key".into(), "app-key".into()),
                ("dropbox.app_secret".into(), "app-secret".into())
            ]
        );
    }

    #[test]
    fn dropbox_oauth_url_requests_offline_code_flow() {
        let url = dropbox_oauth_url("key with spaces");

        assert_eq!(
            url,
            "https://www.dropbox.com/oauth2/authorize?client_id=key%20with%20spaces&response_type=code&token_access_type=offline"
        );
    }

    #[test]
    fn oauth_secrets_prefers_refresh_token() {
        let token = json!({
            "access_token": "access",
            "refresh_token": "refresh",
            "account_id": "dbid:test"
        });

        let secrets = oauth_secrets(&token, "app-key", Some("app-secret")).unwrap();

        assert_eq!(
            secrets,
            vec![
                ("dropbox.refresh_token".into(), "refresh".into()),
                ("dropbox.app_key".into(), "app-key".into()),
                ("dropbox.app_secret".into(), "app-secret".into())
            ]
        );
    }

    #[test]
    fn oauth_secrets_falls_back_to_access_token() {
        let token = json!({
            "access_token": "access"
        });

        let secrets = oauth_secrets(&token, "app-key", None).unwrap();

        assert_eq!(
            secrets,
            vec![
                ("dropbox.access_token".into(), "access".into()),
                ("dropbox.app_key".into(), "app-key".into())
            ]
        );
    }

    #[test]
    fn base64_encode_handles_padding() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
    }

    #[test]
    fn basic_auth_token_encodes_client_credentials() {
        assert_eq!(basic_auth_token("key", "secret"), "a2V5OnNlY3JldA==");
    }

    #[test]
    fn basic_auth_token_form_encodes_client_credentials() {
        assert_eq!(
            basic_auth_token("key:one", "secret/two"),
            "a2V5JTNBb25lOnNlY3JldCUyRnR3bw=="
        );
    }

    #[test]
    fn upload_mode_defaults_to_safe_add() {
        let mode = UploadMode::safe_add();

        assert_eq!(mode.write_mode, "add");
        assert!(!mode.autorename);
        assert!(mode.strict_conflict);
    }

    #[test]
    fn upload_mode_rename_uses_autorename_without_overwrite() {
        let mode = UploadMode::parse("rename").unwrap();

        assert_eq!(mode.write_mode, "add");
        assert!(mode.autorename);
        assert!(mode.strict_conflict);
    }

    #[test]
    fn upload_mode_overwrite_is_explicit() {
        let mode = UploadMode::parse("overwrite").unwrap();

        assert_eq!(mode.write_mode, "overwrite");
        assert!(!mode.autorename);
        assert!(!mode.strict_conflict);
    }

    #[test]
    fn upload_mode_rejects_unknown_values() {
        let err = UploadMode::parse("replace").unwrap_err();

        assert!(err.contains("add"));
        assert!(err.contains("rename"));
        assert!(err.contains("overwrite"));
    }

    #[test]
    fn dropbox_docs_cover_all_commands() {
        let documented: Vec<_> = DropboxPlugin
            .command_docs()
            .iter()
            .map(|doc| doc.command)
            .collect();

        for command in DropboxPlugin.commands() {
            assert!(documented.contains(command));
        }
    }

    #[test]
    fn configured_env_prefers_call_config_over_process_env() {
        let mut env = HashMap::new();
        env.insert("DROPBOX_TOKEN".into(), "call-token".into());

        assert_eq!(
            configured_or_process_env(&env, "DROPBOX_TOKEN").as_deref(),
            Some("call-token")
        );
    }

    #[test]
    fn app_secret_from_env_can_ignore_saved_secret_fallback() {
        let env = HashMap::new();

        assert_eq!(app_secret_from_env_or_secret(&env, false), None);
    }

    #[test]
    fn app_secret_from_env_accepts_explicit_secret_aliases() {
        let mut env = HashMap::new();
        env.insert("DROPBOX_CLIENT_SECRET".into(), "app-secret".into());

        assert_eq!(
            app_secret_from_env_or_secret(&env, false).as_deref(),
            Some("app-secret")
        );
    }

    #[test]
    fn credential_summary_reports_access_token_source() {
        let mut env = HashMap::new();
        env.insert("DROPBOX_TOKEN".into(), "call-token".into());

        let summary = credential_summary(&env);

        assert!(summary.configured);
        assert_eq!(summary.source, "access_token");
    }

    #[test]
    fn credential_summary_reports_refresh_token_source() {
        let mut env = HashMap::new();
        env.insert("DROPBOX_REFRESH_TOKEN".into(), "refresh".into());
        env.insert("DROPBOX_APP_KEY".into(), "app-key".into());

        let summary = credential_summary(&env);

        assert!(summary.configured);
        assert_eq!(summary.source, "refresh_token");
    }

    #[test]
    fn merge_list_folder_page_appends_entries_and_updates_cursor() {
        let mut base = json!({
            "entries": [
                { ".tag": "file", "name": "a.txt" }
            ],
            "cursor": "cursor-1",
            "has_more": true
        });
        let next = json!({
            "entries": [
                { ".tag": "folder", "name": "reports" }
            ],
            "cursor": "cursor-2",
            "has_more": false
        });

        merge_list_folder_page(&mut base, next).unwrap();

        assert_eq!(base["entries"].as_array().unwrap().len(), 2);
        assert_eq!(base["cursor"].as_str(), Some("cursor-2"));
        assert_eq!(base["has_more"].as_bool(), Some(false));
    }

    #[test]
    fn merge_list_folder_page_rejects_missing_entries() {
        let mut base = json!({
            "entries": [],
            "cursor": "cursor-1",
            "has_more": true
        });
        let next = json!({
            "cursor": "cursor-2",
            "has_more": false
        });

        let err = merge_list_folder_page(&mut base, next).unwrap_err();

        assert!(err.contains("entries"));
    }
}
