//! A generic whole-file read/write primitive for `effects::Effect::Fs`.
//! Deliberately narrow: Zen's own `fs.list`/`fs.copy`/`workspace.read`/
//! `pipe_save` builtins each carry their own path-resolution and
//! capability-namespace rules and are left as direct `std::fs` calls in
//! `executor.rs` - collapsing them into this primitive would risk changing
//! their behavior. This module exists for the shape Flux needs (read/write
//! a resolved path as text) and is intentionally reusable by any future Zen
//! builtin that wants the same plain semantics, without being retrofitted
//! onto the existing richer ones.
//!
//! Path resolution (workspace-relative, sandboxing, etc.) and capability
//! checks are the caller's responsibility, same division as `process.rs`:
//! this module performs the syscall, nothing else.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::values::Value;

pub struct FsReadRequest {
    pub path: String,
}

pub struct FsWriteRequest {
    pub path: String,
    pub contents: String,
}

pub fn fs_read(request: FsReadRequest) -> Result<Value, String> {
    fs::read_to_string(&request.path)
        .map(Value::String)
        .map_err(|error| format!("Failed to read '{}': {}", request.path, error))
}

pub fn fs_write(request: FsWriteRequest) -> Result<Value, String> {
    if let Some(parent) = Path::new(&request.path).parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("Failed to create '{}': {}", parent.display(), error))?;
        }
    }
    fs::write(&request.path, &request.contents)
        .map_err(|error| format!("Failed to write '{}': {}", request.path, error))?;

    let mut map = HashMap::new();
    map.insert("path".into(), Value::String(request.path.clone()));
    map.insert("bytes".into(), Value::Number(request.contents.len() as f64));
    Ok(Value::Object(map))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn temp_path(name: &str) -> String {
        env::temp_dir()
            .join(format!("zen-runtime-fs-test-{}-{}", std::process::id(), name))
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn fs_write_then_fs_read_round_trips_real_content() {
        let path = temp_path("roundtrip.txt");

        let write_outcome = fs_write(FsWriteRequest {
            path: path.clone(),
            contents: "hello-fs-effect".into(),
        })
        .unwrap();
        let Value::Object(map) = write_outcome else {
            panic!("expected write outcome object");
        };
        assert!(matches!(map.get("bytes"), Some(Value::Number(n)) if *n == 15.0));

        let read_outcome = fs_read(FsReadRequest { path: path.clone() }).unwrap();
        assert!(matches!(read_outcome, Value::String(s) if s == "hello-fs-effect"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn fs_write_creates_missing_parent_directories() {
        let dir = env::temp_dir().join(format!("zen-runtime-fs-test-{}-nested", std::process::id()));
        let path = dir.join("sub").join("file.txt").to_string_lossy().into_owned();

        fs_write(FsWriteRequest { path: path.clone(), contents: "nested".into() }).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "nested");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn fs_read_missing_file_is_an_error() {
        let path = temp_path("does-not-exist.txt");
        assert!(fs_read(FsReadRequest { path }).is_err());
    }
}
