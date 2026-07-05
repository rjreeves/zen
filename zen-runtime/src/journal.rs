use crate::values::Value;
use std::collections::HashMap;

/// A step's identity, content-addressed rather than a sequence number - see
/// `DURABLE-EXECUTION.md` in the Flux docs. Zen's YAML workflows are a
/// *restricted client*: a step only gets a `StepId` at all if it declares an
/// explicit `checkpoint: "marker"` field (`call_site` is that marker, not
/// the step's `name` - Zen's resume is opt-in per step, not automatic for
/// every named step). `loop_key` is always `None` for Zen - YAML workflows
/// have no loops; it exists so Flux's loop-position addressing fits the same
/// type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StepId {
    pub call_site: String,
    pub loop_key: Option<u64>,
}

impl StepId {
    /// A step participates in the journal only if it has a checkpoint
    /// marker - this is the one place that opt-in rule is encoded.
    pub fn for_checkpoint(checkpoint: &str) -> Self {
        Self {
            call_site: checkpoint.to_string(),
            loop_key: None,
        }
    }
}

/// What happened the last time a step ran, as journaled.
#[derive(Debug, Clone)]
pub enum StepOutcome {
    Done(Value),
    Failed(String),
    Suspended,
}

#[derive(Debug, Clone)]
pub struct StepRecord {
    pub outcome: StepOutcome,
}

/// A durable-function instance - for Zen, a workflow run (`run_id`).
pub struct InstanceId(pub String);

/// A wakeup key for a suspended step. Zen never suspends (no `await`) - this
/// exists so the trait shape matches what Flux needs.
pub struct Signal(pub String);

/// Everything the journal already knows about an instance, keyed by
/// `StepId`. A replay walks its own steps and calls `lookup` per step -
/// unlike Flux (where the *engine* replays compiled code), Zen's engine
/// already owns its own step loop, so `resume` just hands back the
/// previously-recorded state for that loop to consult.
#[derive(Debug, Default)]
pub struct ResumeState {
    pub records: HashMap<StepId, StepRecord>,
}

/// One journal, one replay implementation, shared by a full language (Flux
/// durable functions) and a restricted client (Zen YAML workflows). See
/// `RUNTIME-INTERFACES.md` §5 in the Flux docs.
pub trait Journal {
    fn lookup(&self, id: &StepId) -> Option<StepRecord>;
    fn append(&mut self, id: StepId, outcome: StepOutcome) -> Result<(), String>;
    fn suspend(&mut self, id: StepId, wakeup: Signal) -> Result<(), String>;
    fn resume(&mut self, instance: InstanceId) -> Result<ResumeState, String>;
}
