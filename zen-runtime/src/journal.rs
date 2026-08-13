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
    /// `list_incomplete_instances`) can resume it with no other input - see
    /// `lookup_instance`. Zen has no dispatcher and stubs this to a no-op.
    fn register_instance(
        &mut self,
        instance: InstanceId,
        fn_name: String,
        args: Vec<Value>,
        source: String,
    ) -> Result<(), String>;
    /// Every registered instance not yet marked completed via
    /// `mark_instance_completed` - across *all* instances in this journal,
    /// not scoped to one `InstanceId` the way `append`/`suspend`/
    /// `deliver`/`resume` are. A dispatcher polls this to discover work
    /// with no prior knowledge of which instance/fn/script produced it.
    /// Deliberately **not** "has a row with `status = 'suspended'`" -
    /// `deliver` already flips a delivered step's own row straight to
    /// 'done', independent of whether the durable fn's remaining
    /// orchestration body has actually been re-run past that point. An
    /// instance stays listed here until something explicitly calls
    /// `mark_instance_completed` for it, which only happens once a real
    /// resume genuinely finishes the whole call, not merely once one of
    /// its awaits is delivered.
    fn list_incomplete_instances(&self) -> Result<Vec<InstanceId>, String>;
    /// Durably marks `instance` as fully completed - called once resuming
    /// it genuinely finishes the whole durable fn call (not merely once an
    /// individual awaited signal is delivered: delivering only unblocks
    /// one step, the fn's remaining orchestration body - possibly more
    /// steps, possibly another `await` - still needs to actually run
    /// before the *instance* itself is done). This is what lets
    /// `list_incomplete_instances` tell "still needs resuming" apart from
    /// "delivered but not yet resumed" - the two are not the same thing.
    fn mark_instance_completed(&mut self, instance: InstanceId) -> Result<(), String>;
    /// Looks up what `register_instance` recorded for `instance`, if
    /// anything - `None` if never registered (or this journal has no
    /// dispatcher-facing metadata at all, e.g. Zen).
    fn lookup_instance(&self, instance: &InstanceId) -> Result<Option<RegisteredInstance>, String>;
    /// Attempts to exclusively claim `instance` for the duration of a
    /// replay - closes a real check-then-act race: without this, two
    /// processes racing to resume the same suspended instance can both see
    /// a step as un-journaled (each has its own in-memory cache from its
    /// own `resume` call), both fire its real effects, and whichever
    /// `append`s second silently overwrites the first's journaled result
    /// with no error surfaced anywhere. `Ok(true)` if the lock was
    /// acquired - either freshly, or reclaimed from a holder whose OS
    /// process no longer exists (checked via PID liveness, not a blind
    /// timeout); `Ok(false)` if a live process currently holds it. Zen has
    /// no concurrent-replay story of its own (a single `WorkflowEngine`
    /// process) and stubs this to always succeed.
    fn try_lock_instance(&mut self, instance: &InstanceId) -> Result<bool, String>;
    /// Releases a lock this same process previously acquired via
    /// `try_lock_instance`. Must be called on every exit path - success,
    /// business error, or genuine failure - see `flux-lang`'s
    /// `run_durable_with_outcome`, which guarantees this via ordinary
    /// sequential code (capture the inner call's `Result`, always unlock,
    /// then return it) rather than a `Drop` guard, since a guard holding
    /// `&mut dyn Journal` would conflict with the rest of the function also
    /// needing it mutably.
    fn unlock_instance(&mut self, instance: &InstanceId) -> Result<(), String>;
}
