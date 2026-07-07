use std::fs;
use std::path::PathBuf;

use crate::capabilities::CapabilityGrant;
use crate::process::{exec_command, ExecRequest};
use crate::values::Value;

/// The boundary where a language actually does something to the world.
/// `Process` (unified behind `exec_command`/`ExecRequest`, shared by the
/// `exec` builtin, external-command syntax, the workflow engine, and
/// `postgres.rs`) and `Fs` (below) are wired through `Effects`. `db` needs
/// no separate variant - Zen's own `pg.query`/`pg.dump`/etc. already run
/// through `Process` via `psql`/`pg_dump`, there is no native DB client to
/// extract. `net` is deliberately not here yet: Zen has no generic `net`
/// capability today (only `dropbox.rs`'s own scoped `ureq` calls, gated by
/// plugin-specific permissions, not a shared capability), so there is no
/// existing call site to extract from and no Zen behavior to validate a
/// seam against - see `docs/PHASE3-PLAN.md` in the Flux repo.
pub enum Effect {
    Process(ExecRequest),
    Fs(FsRequest),
}

/// An already-resolved filesystem operation. Deliberately carries a
/// resolved absolute `path`, not a user-supplied string: path resolution
/// (`resolve_workspace_path`/`resolve_local_write_path`) and the capability
/// check both happen in the caller, exactly mirroring how `ExecRequest` is
/// already fully formed - with permissions already checked - before
/// `exec_command` ever sees it. This effect performs the raw operation
/// only; it carries no policy.
pub enum FsRequest {
    Read { path: PathBuf },
    Write { path: PathBuf, contents: String },
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
            Effect::Process(_) => Err("FsEffects does not handle process effects".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::Duration;

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
}
