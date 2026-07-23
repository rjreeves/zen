use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::capabilities::CapabilityGrant;
use crate::net::NetRequest;
use crate::process::{exec_command, ExecRequest};
use crate::values::Value;

/// The boundary where a language actually does something to the world.
/// `Process` (unified behind `exec_command`/`ExecRequest`, shared by the
/// `exec` builtin, external-command syntax, the workflow engine, and
/// `postgres.rs`) and `Fs` (below) are wired through `Effects`. `db` needs
/// no separate variant - Zen's own `pg.query`/`pg.dump`/etc. already run
/// through `Process` via `psql`/`pg_dump`, there is no native DB client to
/// extract. `Net` is a data shape only (`net.rs`) - Zen still has no
/// generic `net` capability of its own (only `dropbox.rs`'s scoped `ureq`
/// calls), so there's no Zen behavior to wire it into; `flux-lang` supplies
/// its own performer with its own `ureq` dependency, since a real HTTP
/// client is explicitly kept out of `zen-runtime` by
/// `EXTRACTION-PLAN.md`'s dependency-footprint guardrail.
pub enum Effect {
    Process(ExecRequest),
    Fs(FsRequest),
    Net(NetRequest),
}

/// An already-resolved filesystem operation. Deliberately carries a
/// resolved absolute `path`, not a user-supplied string: path resolution
/// (`resolve_workspace_path`/`resolve_local_write_path`) and the capability
/// check both happen in the caller, exactly mirroring how `ExecRequest` is
/// already fully formed - with permissions already checked - before
/// `exec_command` ever sees it. This effect performs the raw operation
/// only; it carries no policy.
///
/// `List` is additive, for Flux's `fs_list` (`docs/PHASE3-PLAN.md`'s 3.3
/// generalizing pushdown beyond SQLite) - deliberately **not** the same
/// thing as Zen's own `fs_list`/`fs_list_builtin` in `executor.rs`, which
/// stays untouched (same reasoning as `Read`/`Write` not absorbing
/// `fs_copy`/`workspace.*`: this effect is the narrow "read one file" /
/// "write one file" / "list one directory" primitive, not a port of Zen's
/// richer, workspace-confined surface).
pub enum FsRequest {
    Read { path: PathBuf },
    Write { path: PathBuf, contents: String },
    List { path: PathBuf },
}

/// The outcome of performing an effect: a value, never a panic across the
/// seam. `Ok(result)` or `Err(reason)` - `Result` already is that shape, so
/// this is a named alias rather than a new enum.
pub type EffectOutcome = Result<Value, String>;

/// Performs an effect under a given capability grant. See
/// `RUNTIME-INTERFACES.md` §4 in the Flux docs.
pub trait Effects {
    fn perform(&mut self, effect: Effect, grant: &CapabilityGrant) -> EffectOutcome;
}

/// Runs the process-exec effect via the existing `exec_command`. Stateless;
/// `grant` is accepted (matching the trait Flux needs) but unused here
/// since Zen's permission check already happened via `Capabilities` before
/// the effect was ever constructed.
pub struct ProcessEffects;

impl Effects for ProcessEffects {
    fn perform(&mut self, effect: Effect, _grant: &CapabilityGrant) -> EffectOutcome {
        match effect {
            Effect::Process(request) => exec_command(request),
            Effect::Fs(_) => Err("ProcessEffects does not handle filesystem effects".into()),
            Effect::Net(_) => Err("ProcessEffects does not handle network effects".into()),
        }
    }
}

/// Runs the filesystem effect via `std::fs` directly - no new dependency,
/// matching the project's dependency-footprint guardrail. Stateless for the
/// same reason as `ProcessEffects`: policy (capability check, path
/// resolution/confinement) happens in the caller.
pub struct FsEffects;

impl Effects for FsEffects {
    fn perform(&mut self, effect: Effect, _grant: &CapabilityGrant) -> EffectOutcome {
        match effect {
            Effect::Fs(FsRequest::Read { path }) => fs::read_to_string(&path)
                .map(Value::String)
                .map_err(|error| error.to_string()),
            Effect::Fs(FsRequest::Write { path, contents }) => {
                fs::write(&path, &contents).map_err(|error| error.to_string())?;
                Ok(Value::Bool(true))
            }
            Effect::Fs(FsRequest::List { path }) => list_directory(&path).map_err(|error| error.to_string()),
            Effect::Process(_) => Err("FsEffects does not handle process effects".into()),
            Effect::Net(_) => Err("FsEffects does not handle network effects".into()),
        }
    }
}

/// Lists `path`'s immediate entries as `{name, path, is_dir, size}`. A
/// directory's `size` is a **real recursive sum** of every file under it,
/// computed here (`directory_size` below) - the point is that a caller
/// (Flux) never needs list-iteration/recursion syntax it doesn't have to
/// answer "how big is this folder."
fn list_directory(path: &Path) -> Result<Value, std::io::Error> {
    let mut records = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        let is_dir = metadata.is_dir();
        let size = if is_dir { directory_size(&entry.path())? } else { metadata.len() };

        let mut fields = HashMap::new();
        fields.insert("name".into(), Value::String(entry.file_name().to_string_lossy().into_owned()));
        fields.insert("path".into(), Value::String(entry.path().to_string_lossy().into_owned()));
        fields.insert("is_dir".into(), Value::Bool(is_dir));
        fields.insert("size".into(), Value::Number(size as f64));
        records.push(Value::Object(fields));
    }
    Ok(Value::List(records))
}

/// The first error anywhere in the subtree aborts the whole listing (e.g. a
/// permission-denied nested folder) rather than silently skipping it -
/// matches `Read`/`Write`'s existing propagate-don't-swallow handling.
fn directory_size(path: &Path) -> Result<u64, std::io::Error> {
    let mut total = 0u64;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            total += directory_size(&entry.path())?;
        } else {
            total += metadata.len();
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::Duration;

    fn field<'a>(entry: &'a Value, key: &str) -> &'a Value {
        match entry {
            Value::Object(map) => map.get(key).unwrap_or_else(|| panic!("missing field '{key}'")),
            other => panic!("expected an object entry, got {other:?}"),
        }
    }

    fn echo_request(text: &str) -> ExecRequest {
        ExecRequest {
            command: format!("echo {}", text),
            argv: None,
            attempts: 1,
            timeout: Some(Duration::from_secs(10)),
            wait_children: false,
            workdir: None,
            env: HashMap::new(),
            secret_values: Vec::new(),
        }
    }

    #[test]
    fn process_effects_performs_process_effect() {
        let outcome = ProcessEffects.perform(
            Effect::Process(echo_request("hello-effects")),
            &CapabilityGrant::new("proc.exec"),
        );

        let Value::Object(map) = outcome.unwrap() else {
            panic!("Expected exec output object");
        };
        assert!(matches!(map.get("success"), Some(Value::Bool(true))));
        match map.get("stdout") {
            Some(Value::String(value)) => assert!(value.contains("hello-effects")),
            other => panic!("Expected stdout string, got {:?}", other),
        }
    }

    #[test]
    fn process_effects_rejects_fs_effect() {
        let outcome = ProcessEffects.perform(
            Effect::Fs(FsRequest::Read { path: PathBuf::from("unused") }),
            &CapabilityGrant::new("fs.read"),
        );
        assert!(outcome.is_err());
    }

    #[test]
    fn fs_effects_writes_then_reads_a_real_file() {
        let dir = std::env::temp_dir().join(format!("zen-runtime-fs-effect-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("effect.txt");

        let write_outcome = FsEffects.perform(
            Effect::Fs(FsRequest::Write { path: path.clone(), contents: "hello-fs-effect".into() }),
            &CapabilityGrant::new("fs.write"),
        );
        assert!(matches!(write_outcome, Ok(Value::Bool(true))));

        let read_outcome =
            FsEffects.perform(Effect::Fs(FsRequest::Read { path: path.clone() }), &CapabilityGrant::new("fs.read"));
        match read_outcome {
            Ok(Value::String(contents)) => assert_eq!(contents, "hello-fs-effect"),
            other => panic!("Expected read-back string, got {:?}", other),
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fs_effects_rejects_process_effect() {
        let outcome = FsEffects.perform(
            Effect::Process(echo_request("unused")),
            &CapabilityGrant::new("proc.exec"),
        );
        assert!(outcome.is_err());
    }

    #[test]
    fn fs_effects_read_reports_missing_file() {
        let outcome = FsEffects.perform(
            Effect::Fs(FsRequest::Read { path: PathBuf::from("this-path-should-not-exist-anywhere") }),
            &CapabilityGrant::new("fs.read"),
        );
        assert!(outcome.is_err());
    }

    #[test]
    fn fs_effects_list_reports_entries_with_real_recursive_directory_sizes() {
        let root = std::env::temp_dir().join(format!("zen-runtime-fs-effect-list-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("subdir")).unwrap();
        fs::write(root.join("file.txt"), "12345").unwrap(); // 5 bytes
        fs::write(root.join("subdir").join("nested.txt"), "1234567890").unwrap(); // 10 bytes

        let outcome = FsEffects.perform(Effect::Fs(FsRequest::List { path: root.clone() }), &CapabilityGrant::new("fs.read"));
        let Ok(Value::List(entries)) = outcome else {
            panic!("expected a list, got {outcome:?}");
        };
        assert_eq!(entries.len(), 2);

        let dir_entry = entries
            .iter()
            .find(|e| matches!(field(e, "name"), Value::String(s) if s == "subdir"))
            .expect("subdir entry");
        assert!(matches!(field(dir_entry, "is_dir"), Value::Bool(true)));
        // subdir's own on-disk size is 0 bytes - this must be the real
        // recursive sum of nested.txt inside it.
        assert!(matches!(field(dir_entry, "size"), Value::Number(n) if *n == 10.0));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fs_effects_list_missing_directory_is_an_error() {
        let outcome = FsEffects.perform(
            Effect::Fs(FsRequest::List { path: PathBuf::from("this-dir-should-not-exist-anywhere") }),
            &CapabilityGrant::new("fs.read"),
        );
        assert!(outcome.is_err());
    }

    #[test]
    fn neither_existing_performer_handles_a_net_effect() {
        let net_request = || NetRequest {
            method: "GET".into(),
            url: "http://unused.invalid".into(),
            headers: Vec::new(),
            body: None,
            timeout: None,
        };
        let grant = CapabilityGrant::new("net");

        assert!(ProcessEffects.perform(Effect::Net(net_request()), &grant).is_err());
        assert!(FsEffects.perform(Effect::Net(net_request()), &grant).is_err());
    }
}
