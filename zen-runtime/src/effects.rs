use crate::capabilities::CapabilityGrant;
use crate::fs::{fs_read, fs_write, FsReadRequest, FsWriteRequest};
use crate::net::NetRequest;
use crate::process::{exec_command, ExecRequest};
use crate::values::Value;

/// The boundary where a language actually does something to the world.
/// `Process` (subprocess exec) and `Fs` (whole-file read/write, see `fs.rs`)
/// have real performers here (`ProcessEffects`/`FsEffects`), both needing
/// only `std`. `Net` is a data shape only - `net.rs` explains why no
/// zen-runtime-resident performer exists for it. `Db` is a separate, larger
/// follow-on - see `docs/PHASE3-PLAN.md`'s sub-phase 3.0 in the Flux repo.
pub enum Effect {
    Process(ExecRequest),
    Fs(FsEffect),
    Net(NetRequest),
}

/// `Fs`'s two operations, nested inside `Effect::Fs` rather than as two
/// top-level `Effect` variants - keeps "this is a filesystem effect" and
/// "which filesystem operation" as separate match decisions for a performer.
pub enum FsEffect {
    Read(FsReadRequest),
    Write(FsWriteRequest),
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
/// `grant` is accepted (matching the trait Flux needs) but unused here since
/// Zen's permission check already happened via `Capabilities` before the
/// effect was ever constructed. Errors (rather than panics) on an `Effect::Fs`
/// - each performer handles one effect kind; callers pick the matching one.
pub struct ProcessEffects;

impl Effects for ProcessEffects {
    fn perform(&mut self, effect: Effect, _grant: &CapabilityGrant) -> EffectOutcome {
        match effect {
            Effect::Process(request) => exec_command(request),
            Effect::Fs(_) => Err("ProcessEffects cannot perform a Fs effect".into()),
            Effect::Net(_) => Err("ProcessEffects cannot perform a Net effect".into()),
        }
    }
}

/// Runs the filesystem-read/write effect via `fs::fs_read`/`fs::fs_write`.
/// Stateless and grant-unused for the same reason as `ProcessEffects`; path
/// resolution and the `fs.read`/`fs.write` permission check are the
/// caller's job, not this performer's - see `fs.rs`'s module doc.
pub struct FsEffects;

impl Effects for FsEffects {
    fn perform(&mut self, effect: Effect, _grant: &CapabilityGrant) -> EffectOutcome {
        match effect {
            Effect::Fs(FsEffect::Read(request)) => fs_read(request),
            Effect::Fs(FsEffect::Write(request)) => fs_write(request),
            Effect::Process(_) => Err("FsEffects cannot perform a Process effect".into()),
            Effect::Net(_) => Err("FsEffects cannot perform a Net effect".into()),
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
    fn fs_effects_performs_write_then_read_effect() {
        use std::env;
        use std::fs;

        let path = env::temp_dir()
            .join(format!("zen-runtime-effects-test-{}.txt", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let grant = CapabilityGrant::new("fs.write");

        let write_outcome = FsEffects.perform(
            Effect::Fs(FsEffect::Write(FsWriteRequest { path: path.clone(), contents: "hello-fs-effects".into() })),
            &grant,
        );
        assert!(matches!(write_outcome, Ok(Value::Object(_))));

        let read_outcome = FsEffects.perform(Effect::Fs(FsEffect::Read(FsReadRequest { path: path.clone() })), &grant);
        assert!(matches!(read_outcome, Ok(Value::String(s)) if s == "hello-fs-effects"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn process_effects_rejects_fs_effect_and_vice_versa() {
        let grant = CapabilityGrant::new("fs.read");
        let mismatched = ProcessEffects.perform(
            Effect::Fs(FsEffect::Read(FsReadRequest { path: "unused".into() })),
            &grant,
        );
        assert!(mismatched.is_err());

        let mismatched = FsEffects.perform(Effect::Process(echo_request("unused")), &grant);
        assert!(mismatched.is_err());
    }

    #[test]
    fn neither_existing_performer_handles_a_net_effect() {
        use crate::net::NetRequest;

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
