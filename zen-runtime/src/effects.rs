use crate::capabilities::CapabilityGrant;
use crate::process::{exec_command, ExecRequest};
use crate::values::Value;

/// The boundary where a language actually does something to the world.
/// Intentionally partial: only the subprocess-exec case (already unified
/// behind `exec_command`/`ExecRequest`, shared by the `exec` builtin,
/// external-command syntax, the workflow engine, and `postgres.rs`) is
/// wired through `Effects` so far. `fs`/`db`/`net` effects are a separate,
/// larger follow-on - see `EXTRACTION-PLAN.md` Stage 3 in the Flux docs.
pub enum Effect {
    Process(ExecRequest),
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

/// The only `Effects` implementor today - runs the process-exec effect via
/// the existing `exec_command`. Stateless; `grant` is accepted (matching the
/// trait Flux needs) but unused here since Zen's permission check already
/// happened via `Capabilities` before the effect was ever constructed.
pub struct ProcessEffects;

impl Effects for ProcessEffects {
    fn perform(&mut self, effect: Effect, _grant: &CapabilityGrant) -> EffectOutcome {
        match effect {
            Effect::Process(request) => exec_command(request),
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
}
