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

/// Everything `register_instance` durably recorded about an instance at
/// its first registration - what a dispatcher needs to auto-discover and
/// resume a suspended instance knowing nothing but its `InstanceId`. The
/// full script *source* (not a path) is captured, not just referenced, so
/// resume always replays the exact text that produced this journal's
/// existing rows, even if the source file on disk changes later.
#[derive(Debug, Clone)]
pub struct RegisteredInstance {
    pub fn_name: String,
    pub args: Vec<Value>,
    pub source: String,
}

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
    /// Deliver a value to whichever suspended step is waiting on `signal`
    /// (manual delivery only - no automatic dispatcher/event-bus wakeup
    /// yet; a caller re-invokes the durable fn after calling this). `None`
    /// if no suspended step matches; `Some(id)` names the step now marked
    /// done.
    fn deliver(&mut self, signal: Signal, value: Value) -> Result<Option<StepId>, String>;
    fn resume(&mut self, instance: InstanceId) -> Result<ResumeState, String>;
    /// Durably records which script/fn/args produced `instance`, the first
    /// time it's seen - idempotent (`INSERT OR IGNORE`: a caller
    /// re-registering the same instance with different args/fn_name is a
    /// caller bug, not a legitimate update, so first write wins). Exists so
    /// a dispatcher that only knows an `InstanceId` (from
    /// `list_suspended_instances`) can resume it with no other input - see
    /// `lookup_instance`. Zen has no dispatcher and stubs this to a no-op.
    fn register_instance(
        &mut self,
        instance: InstanceId,
        fn_name: String,
        args: Vec<Value>,
        source: String,
    ) -> Result<(), String>;
    /// Every instance currently suspended (has at least one row with
    /// `status = 'suspended'`) - across *all* instances in this journal,
    /// not scoped to one `InstanceId` the way `append`/`suspend`/
    /// `deliver`/`resume` are. A dispatcher polls this to discover work
    /// with no prior knowledge of which instance/fn/script produced it. An
    /// instance never reappears here once fully completed: an awaiting
    /// caller only gets back a completed result once every await it passed
    /// through already has a `Done` record, so a completed run can never
    /// still own a `status='suspended'` row.
    fn list_suspended_instances(&self) -> Result<Vec<InstanceId>, String>;
    /// Looks up what `register_instance` recorded for `instance`, if
    /// anything - `None` if never registered (or this journal has no
    /// dispatcher-facing metadata at all, e.g. Zen).
    fn lookup_instance(&self, instance: &InstanceId) -> Result<Option<RegisteredInstance>, String>;
}
