use crate::ast::{Expr, FunctionCall};
use crate::runtime::executor::Executor;
use crate::runtime::plugin::{CommandDoc, PluginResult, ZenPlugin};
use crate::runtime::values::Value;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use zip::write::FileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

pub struct ArchivePlugin;

static ARCHIVE_DOCS: &[CommandDoc] = &[
    CommandDoc {
        command: "archive.zip",
        summary: "Creates a zip archive from files or directories in the workspace.",
        usage: "archive.zip <zip-path> <path...> [exclude <path...>]",
        examples: &[
            r#"archive.zip "dist/zen-win64.zip" "dist/zen.exe" "dist/README.md" ".zen""#,
            r#"archive.zip "dist/zen-win64.zip" "dist/zen.exe" ".zen" exclude ".zen/runtime.db""#,
        ],
    },
    CommandDoc {
        command: "archive.list",
        summary: "Lists entries in a zip archive.",
        usage: "archive.list <zip-path>",
        examples: &[r#"archive.list "dist/zen-win64.zip" | echo table"#],
    },
];

impl ZenPlugin for ArchivePlugin {
    fn name(&self) -> &'static str {
        "archive"
    }

    fn commands(&self) -> &'static [&'static str] {
        &["archive.zip", "archive.list"]
    }

    fn command_permissions(&self) -> &'static [(&'static str, &'static str)] {
        &[
            ("archive.zip", "fs.read"),
            ("archive.zip", "fs.write"),
            ("archive.list", "fs.read"),
        ]
    }

    fn command_docs(&self) -> &'static [CommandDoc] {
        ARCHIVE_DOCS
    }

    fn call(
        &self,
        executor: &mut Executor,
        call: &FunctionCall,
        _input: &Value,
    ) -> Result<PluginResult, String> {
        let result = match call.name.join(".").as_str() {
            "archive.zip" => archive_zip(executor, call.args.clone()),
            "archive.list" => archive_list(executor, call.args.clone()),
            _ => return Ok(PluginResult::unhandled()),
        }?;

        Ok(PluginResult::handled(result))
    }
}

fn archive_zip(executor: &mut Executor, args: Vec<Expr>) -> Result<Value, String> {
    executor.check_permission("fs.read")?;
    executor.check_permission("fs.write")?;

    if args.len() < 2 {
        return Err("archive.zip expects <zip-path> <path...>".into());
    }

    let mut args = args;
    let zip_path = value_to_string(executor.plugin_arg_value(args.remove(0))?);
    let zip_path = executor.resolve_local_write_path(&zip_path)?;
    if let Some(parent) = zip_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to create '{}': {}", parent.display(), error))?;
    }

    let mut raw_args = Vec::new();
    for arg in args {
        let raw = value_to_string(executor.plugin_arg_value(arg)?);
        raw_args.push(raw);
    }
    let (source_args, exclude_args) = split_archive_zip_args(raw_args)?;

    let mut sources = Vec::new();
    for raw in source_args {
        let resolved = executor.resolve_workspace_path(&raw)?;
        sources.push((raw, resolved));
    }
    let excludes: Vec<_> = exclude_args
        .iter()
        .map(|exclude| normalize_archive_path(exclude))
        .collect();

    let file = File::create(&zip_path)
        .map_err(|error| format!("Failed to create '{}': {}", zip_path.display(), error))?;
    let mut writer = ZipWriter::new(file);
    let options = FileOptions::default().compression_method(CompressionMethod::Deflated);
    let mut entries = Vec::new();
    let mut skipped = Vec::new();

    for (raw, source) in sources {
        if source.is_file() {
            let entry_name = archive_entry_name_for_file(&raw);
            add_file(
                &mut writer,
                &source,
                &entry_name,
                &excludes,
                options,
                &mut entries,
                &mut skipped,
            )?;
        } else if source.is_dir() {
            let root_name = archive_root_name(&raw, &source);
            add_directory(
                &mut writer,
                &source,
                &root_name,
                &excludes,
                options,
                &mut entries,
                &mut skipped,
            )?;
        } else {
            return Err(format!(
                "Archive source '{}' was not found",
                source.display()
            ));
        }
    }

    writer
        .finish()
        .map_err(|error| format!("Failed to finish zip '{}': {}", zip_path.display(), error))?;

    let mut map = HashMap::new();
    map.insert("success".into(), Value::Bool(true));
    map.insert("path".into(), Value::String(zip_path.display().to_string()));
    map.insert("entries".into(), Value::List(entries));
    map.insert("skipped".into(), Value::List(skipped));
    Ok(Value::Object(map))
}

fn archive_list(executor: &mut Executor, args: Vec<Expr>) -> Result<Value, String> {
    executor.check_permission("fs.read")?;

    if args.len() != 1 {
        return Err("archive.list expects <zip-path>".into());
    }

    let mut args = args;
    let zip_path = value_to_string(executor.plugin_arg_value(args.remove(0))?);
    let zip_path = executor.resolve_workspace_path(&zip_path)?;
    let file = File::open(&zip_path)
        .map_err(|error| format!("Failed to open '{}': {}", zip_path.display(), error))?;
    let mut archive = ZipArchive::new(file)
        .map_err(|error| format!("Failed to read zip '{}': {}", zip_path.display(), error))?;

    let mut entries = Vec::new();
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|error| format!("Failed to read zip entry {}: {}", index, error))?;
        let mut map = HashMap::new();
        map.insert("name".into(), Value::String(entry.name().into()));
        map.insert("size".into(), Value::Number(entry.size() as f64));
        map.insert(
            "compressed_size".into(),
            Value::Number(entry.compressed_size() as f64),
        );
        map.insert("directory".into(), Value::Bool(entry.is_dir()));
        entries.push(Value::Object(map));
    }

    Ok(Value::List(entries))
}

fn add_directory(
    writer: &mut ZipWriter<File>,
    directory: &Path,
    root_name: &str,
    excludes: &[String],
    options: FileOptions,
    entries: &mut Vec<Value>,
    skipped: &mut Vec<Value>,
) -> Result<(), String> {
    let mut stack = vec![directory.to_path_buf()];

    while let Some(path) = stack.pop() {
        let children = match fs::read_dir(&path) {
            Ok(children) => children,
            Err(error) => {
                skipped.push(skipped_entry(&path, &error.to_string()));
                continue;
            }
        };

        for child in children {
            let child = match child {
                Ok(child) => child,
                Err(error) => {
                    skipped.push(skipped_entry(&path, &error.to_string()));
                    continue;
                }
            };
            let child_path = child.path();
            if child_path.is_dir() {
                stack.push(child_path);
            } else if child_path.is_file() {
                let relative = child_path
                    .strip_prefix(directory)
                    .map_err(|error| format!("Failed to build archive path: {}", error))?;
                let entry_name = path_to_zip_name(Path::new(root_name).join(relative));
                add_file(
                    writer,
                    &child_path,
                    &entry_name,
                    excludes,
                    options,
                    entries,
                    skipped,
                )?;
            }
        }
    }

    Ok(())
}

fn add_file(
    writer: &mut ZipWriter<File>,
    source: &Path,
    entry_name: &str,
    excludes: &[String],
    options: FileOptions,
    entries: &mut Vec<Value>,
    skipped: &mut Vec<Value>,
) -> Result<(), String> {
    if archive_path_is_excluded(entry_name, excludes) {
        skipped.push(skipped_entry(source, "excluded"));
        return Ok(());
    }

    let mut file = match File::open(source) {
        Ok(file) => file,
        Err(error) => {
            skipped.push(skipped_entry(source, &error.to_string()));
            return Ok(());
        }
    };
    let mut buffer = Vec::new();
    if let Err(error) = file.read_to_end(&mut buffer) {
        skipped.push(skipped_entry(source, &error.to_string()));
        return Ok(());
    }

    writer
        .start_file(entry_name, options)
        .map_err(|error| format!("Failed to add '{}' to zip: {}", entry_name, error))?;
    writer
        .write_all(&buffer)
        .map_err(|error| format!("Failed to write '{}' to zip: {}", entry_name, error))?;
    entries.push(entry_value(entry_name, buffer.len() as u64));
    Ok(())
}

fn split_archive_zip_args(args: Vec<String>) -> Result<(Vec<String>, Vec<String>), String> {
    let mut sources = Vec::new();
    let mut excludes = Vec::new();
    let mut in_excludes = false;

    for arg in args {
        if arg == "exclude" {
            if in_excludes {
                return Err("archive.zip exclude marker can only appear once".into());
            }
            in_excludes = true;
            continue;
        }

        if in_excludes {
            excludes.push(arg);
        } else {
            sources.push(arg);
        }
    }

    if sources.is_empty() {
        return Err("archive.zip expects at least one source path".into());
    }

    Ok((sources, excludes))
}

fn archive_path_is_excluded(entry_name: &str, excludes: &[String]) -> bool {
    let entry_name = normalize_archive_path(entry_name);
    excludes
        .iter()
        .any(|exclude| entry_name == *exclude || entry_name.starts_with(&format!("{}/", exclude)))
}

fn normalize_archive_path(path: &str) -> String {
    path.replace('\\', "/")
        .trim_start_matches("./")
        .trim_end_matches('/')
        .into()
}

fn archive_entry_name_for_file(raw: &str) -> String {
    let normalized = normalize_archive_path(raw);
    if normalized.starts_with("dist/") {
        normalized
            .rsplit('/')
            .next()
            .unwrap_or(normalized.as_str())
            .into()
    } else {
        normalized.trim_start_matches("./").into()
    }
}

fn archive_root_name(raw: &str, source: &Path) -> String {
    let normalized = normalize_archive_path(raw);
    normalized
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty() && *name != ".")
        .map(str::to_string)
        .or_else(|| {
            source
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "archive".into())
}

fn path_to_zip_name(path: PathBuf) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn entry_value(name: &str, size: u64) -> Value {
    let mut map = HashMap::new();
    map.insert("name".into(), Value::String(name.into()));
    map.insert("size".into(), Value::Number(size as f64));
    Value::Object(map)
}

fn skipped_entry(path: &Path, reason: &str) -> Value {
    let mut map = HashMap::new();
    map.insert("path".into(), Value::String(path.display().to_string()));
    map.insert("reason".into(), Value::String(reason.into()));
    Value::Object(map)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::FunctionCall;
    use crate::permissions::PermissionSet;

    #[test]
    fn archive_plugin_ignores_unknown_calls() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = ArchivePlugin
            .call(
                &mut executor,
                &FunctionCall {
                    name: vec!["not_archive".into()],
                    args: Vec::new(),
                    config: None,
                },
                &Value::Null,
            )
            .unwrap();

        assert!(!result.is_handled());
    }

    #[test]
    fn archive_excludes_exact_paths_and_children() {
        let excludes = vec![".zen/runtime.db".into(), ".zen/private".into()];

        assert!(archive_path_is_excluded(".zen/runtime.db", &excludes));
        assert!(archive_path_is_excluded(
            ".zen/private/secret.txt",
            &excludes
        ));
        assert!(!archive_path_is_excluded(".zen/startup.fg", &excludes));
    }

    #[test]
    fn archive_zip_args_split_sources_and_excludes() {
        let (sources, excludes) = split_archive_zip_args(vec![
            "dist/zen.exe".into(),
            ".zen".into(),
            "exclude".into(),
            ".zen/runtime.db".into(),
        ])
        .unwrap();

        assert_eq!(sources, vec!["dist/zen.exe", ".zen"]);
        assert_eq!(excludes, vec![".zen/runtime.db"]);
    }
}
