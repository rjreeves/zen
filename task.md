# Task: Backfill Phase 0 toward the Flux-shared runtime traits

## Goal

`zen-runtime` is meant to eventually be shared between Zen (`.fg` + YAML workflows, this
repo) and a future language, Flux (separate repo:
`C:\Users\rreev\OneDrive\Documents\GitHub\Flux\docs`). That doc set's `EXTRACTION-PLAN.md`
sequences the work as Phase 0 (seams, no crate split) → Phase 1 (physically extract
`zen-runtime`) → Phase 2 (port `.fg` onto the traits) → Phase 3 (build Flux).

This repo's own `zen-runtime` extraction (workflow engine, `permissions.rs`, `values.rs`,
`process.rs`, `ScriptRunner`/`SecretStore`/`WorkflowHost`) predates discovering that doc
set, so it satisfies Phase 1's spirit (crate split, dependency-footprint guardrail matches
exactly) but left Phase 0's trait layer mostly unbuilt. This task backfilled that layer,
staged as independent, behavior-preserving increments (per the plan's own working
discipline: "seams before boundaries," "working binary at every commit").

**All 5 stages are now done** - Phase 0 (0.1-0.4) is complete: `ScriptRunner`,
`Capabilities`, `EventSink`, `Effects`, `Journal`, and a Flux-shaped `PluginHost` all exist
in `zen-runtime`, each proven against `Executor` with no observable `.fg`/YAML behavior
change. What's left before Phase 2 ("port `.fg` onto the traits") is a separate,
not-yet-planned effort: actually routing `.fg`'s own execution through these traits (today
they're proven additively, alongside the existing code paths, not yet the only path).

## Stages

| Stage | What | Status |
|---|---|---|
| 1 | `Capabilities` trait (`zen-runtime/src/capabilities.rs`) wrapping `PermissionSet`; `time`/`rand` added as auto-granted capability kinds | Done (commit `adbaa6d`) |
| 2 | `EventSink` trait (`zen-runtime/src/events.rs`); `WorkflowPersistence` implements it, `push_workflow_event`'s SQL write routes through `emit()` | Done (commit `91fdbcf`) |
| 3 | `Effects` trait (`zen-runtime/src/effects.rs`), scoped to the one effect kind already unified behind `exec_command`/`ExecRequest` (subprocess exec). Wired into the `exec` builtin, external-command syntax, the workflow engine, and `postgres.rs` | Done (commit `a8ec010`) |
| 4 | `Journal` trait (`zen-runtime/src/journal.rs`) - reshaped `WorkflowPersistence` into `lookup`/`append`/`suspend`/`resume`. Corrected the doc's literal mapping along the way: Zen's resume is opt-in per step, so `StepId::call_site` is a step's `checkpoint` marker, not its `name` - a step with no checkpoint never touches the journal, matching pre-existing behavior exactly | Done (commit `1b51df1`) |
| 5 | New `PluginHost` trait (`zen-runtime/src/plugin_host.rs`) completing Phase 0.2 - the 3-method capability shape (`use_capability`/`emit`/`secret`), `EventSink`/`SecretStore` as supertraits. Auditing actual plugin call sites first revealed the original "narrow the existing `PluginHost`, touches every plugin file" framing was wrong: only 4 of its ~50 methods (`check_permission`, `plugin_arg_value`, `resolve_workspace_path`, `resolve_local_write_path`) are genuinely shared across diverse plugins; the other ~46 are each called by exactly one dedicated shim plugin proxying straight to `Executor`'s own interpreter-session state, with no Flux-shareable equivalent. So this stage added a small new trait alongside the existing one instead, additive like Stages 1-3 - zero plugin files touched, existing `runtime::plugin::PluginHost`/`ZenPlugin` untouched | Done (commit `baa7eef`) |

## Phase 2 - port `.fg` onto the traits (validate the seam with a real language)

**2.2 (map YAML workflows onto `Journal`) is done** - it's what Stage 4 already built.
Verified against the *literal* named cookbook workflows this time, not just the unit
tests: extracted `resumable-build.yaml` and `postgres-smart-backup.yaml` from
`book/the-zen-automation-cookbook.md` into real files under `examples/` (they were
referenced by the cookbook as "checked-in" but didn't actually exist on disk), ran all
three cookbook workflows (`Scripts/zen-release-build.yaml` validated via `zen explain`
only - it bumps the project's own version number, so wasn't executed for real) through
the built CLI. `resumable-build.yaml` ran a real `cargo build --release`, then a `zen
resume` on the completed run correctly skipped both checkpointed steps and restored their
original output (including the full captured build log) without re-running the build.
`postgres-smart-backup.yaml` ran against no local Postgres server and behaved exactly as
documented: detected passwordless auth wasn't ready, branched via the `if:` condition,
printed the explain step, and skipped the dump step.

**2.1 ("every `.fg` runtime interaction goes through `RuntimeContext` traits") surfaced two
composition problems. Both are now fixed rather than left as findings:**

1. **`RuntimeContext`'s separate-fields shape didn't construct when one type backs
   multiple bundled traits.** `RUNTIME-INTERFACES.md`'s illustrative `RuntimeContext` holds
   five independent `&mut dyn Trait` fields (`caps`, `effects`, `journal`, `plugins`,
   `events`). For `Executor` - which implements `PluginHost`, `EventSink`, and
   `SecretStore` directly, while `Capabilities` is only reachable through its
   `permissions` field - holding `plugins: &mut dyn PluginHost` (a borrow of the whole
   `Executor`) and `caps: &mut dyn Capabilities` (a borrow of `self.permissions`, a field
   of that same `Executor`) at the same time is a real aliasing conflict, not a style
   choice. **Fixed** in `zen-runtime/src/runtime_context.rs`: since `PluginHost` (Stage 5)
   already bundles `use_capability`/`emit`/`secret` behind one handle, `caps`/`events`
   collapse into `plugins` rather than staying separate fields; `journal`/`effects` stay
   separate since they're genuinely backed by different objects
   (`WorkflowPersistence`/stateless `ProcessEffects`) that never alias `plugins`. Proved
   against the real `Executor` (`runtime_context_constructs_for_real_executor` in
   `executor.rs`'s test module), not just a stub - `Executor` is the concrete type that
   made the illustrative shape impossible to build in the first place.
2. **Routing an existing concrete-module call through a shared trait wasn't small at
   first look, but was still tractable.** `dropbox.rs`'s `secret_value()` and its callers
   (`access_token_from_env`, `credential_summary`, `app_key_from_env_or_secret`,
   `app_secret_from_env_or_secret`) were free functions several layers deep with no host
   parameter at all. Separately, `secrets.rs`'s own plugin functions already receive a
   host handle, but typed as the *existing*, wide `runtime::plugin::PluginHost`, which had
   no `read_secret` method. **Fixed**: made `SecretStore` a supertrait of the wide
   `PluginHost` (`src/runtime/plugin.rs`) - free, since `Executor` is `PluginHost`'s only
   implementor and already implements `SecretStore` separately, same pattern
   `WorkflowHost` already uses for its own supertraits - which gives every existing
   `&mut dyn PluginHost` call site `.read_secret(name)` with no upcasting needed. Then
   threaded an `executor: &dyn PluginHost` parameter through `dropbox.rs`'s five-function
   chain, replacing the direct `crate::runtime::plugins::secrets::read_secret` call with
   `executor.read_secret(name)`.

**Still deliberately deferred:** the full "every call site in `executor.rs` takes
`RuntimeContext`" rewrite. Given Zen has exactly one implementation of each trait today,
an exhaustive sweep wouldn't surface a *new* gap beyond the two above (now fixed) - the
real test needs a second caller (Flux) to be meaningful, matching the docs' own "design
Flux-first, validate with Zen" ordering.

## Phase 3.0 - Fs effect (Flux prerequisite)

Flux's Phase 3.2 (static capability checking + the `Effects` seam, in the separate `Flux` repo)
needed `Effect` to cover more than process exec. Investigated all three gaps
(`docs/PHASE3-PLAN.md`'s "3.0 - Prerequisite runtime work" in the Flux repo) before writing any
code, since the plan doc's framing ("extend `Effect` with `Fs`/`Net`/`Db`, following the existing
`ProcessEffects` pattern") turned out not to fit all three the same way:

- **Db needed nothing.** `postgres.rs` already imports `zen_runtime::effects::{Effect, Effects,
  ProcessEffects}` directly - `pg.query`/`pg.dump`/`pg.restore` all shell out via `psql`/`pg_dump`
  through the existing `Process` effect. No native Postgres client to extract.
- **Net was skipped**, on purpose, after finding there's no generic `net` capability in Zen at
  all today - only `dropbox.rs`'s own `ureq` calls, gated by plugin-scoped permissions
  (`dropbox.read`/`dropbox.write`), never a shared capability reaching `.fg` scripts. Adding
  `Effect::Net` now would be greenfield design with no existing Zen behavior to validate the seam
  against, and a real implementor would need `ureq`, which the project's own dependency-footprint
  guardrail keeps out of `zen-runtime`. Deferred until there's an actual caller (Flux, or a real
  Zen feature) to design it against.
- **Fs got a real, narrow seam.** Unlike `Process` (already unified behind one `exec_command`
  before extraction), filesystem access was scattered across ~6 call sites (`workspace_read`,
  `fs_copy_builtin`/`fs_copy`/`fs_copy_file`, `fs_list_builtin`/`fs_list`,
  `workspace_entries`/`workspace_find_files`), each with its own path-resolution/confinement
  logic (`resolve_workspace_path`, `resolve_local_write_path`, `resolve_fs_path`). Rather than
  redesigning all of that, added `Effect::Fs(FsRequest::{Read, Write})` carrying an
  **already-resolved** path - mirroring `ExecRequest`, which is also fully resolved (permissions
  checked, command built) before `exec_command` ever sees it. Wired into exactly the two call
  sites whose shape already matched that contract cleanly: `workspace_read` (read) and
  `pipe_save` (write) - both already resolved a path and checked a capability before touching
  `std::fs` directly. `fs_copy`/`fs_list`/the `workspace.*` family are explicitly **not** touched;
  they carry more logic (multi-item copy, directory listing, confinement to a workspace root) that
  doesn't reduce to "read one file" / "write one file" without a bigger redesign - left as
  possible follow-on, not something this stage needed to unblock Flux.

Error text preserved exactly: both call sites already built their own error strings (e.g. `Failed
to read '{}': {}`) from the *original* user-supplied path, not the resolved one. `FsEffects`
returns only the raw underlying `io::Error` text; both call sites still wrap it in their original
format string, so `.fg` error messages are byte-for-byte unchanged. Verified with
`cargo test --workspace < /dev/null`: all 279 `zen` + 19 `zen-runtime` tests pass unchanged,
including `workspace_read_reads_text_file`, `workspace_read_rejects_parent_traversal`, and
`save_writes_pipeline_input_to_workspace_file` - the exact tests covering the two touched call
sites.

## Design principles (carried from the Flux docs)

- Shape traits for Flux's more demanding needs; validate that Zen's simpler case still
  works. Don't shape `Journal` around Zen's YAML model.
- Translate line-for-line where possible - don't rewrite ported subsystem behavior.
- Every stage must leave `cargo build --workspace` and `cargo test --workspace` green,
  with no observable behavior change for `.fg`/YAML.
- `zen-runtime` must never reference a concrete interpreter type or Flux type.

## Verification per stage

`cargo build --workspace`, then `cargo test --workspace < /dev/null` (stdin must be closed
- a pre-existing test reads the repo's real `.zen/startup.fg` and blocks on an interactive
permission prompt otherwise). Add a unit test for the new trait's implementation directly,
plus a manual smoke test through the real CLI (`cargo run -- run <script>`) exercising the
behavior the stage touched.

## Phase 3.0 follow-up - List and Net, on top of this section's Fs work

A separate, independent session had picked up the same "extend `Effect`" work in parallel
(unaware of this one) and landed `Fs(FsRequest::{Read, Write})` plus generalized database
pushdown in the Flux repo before this history existed here - discovered only when reconciling
the two afterward. Rather than merge two incompatible `Fs` shapes, **this section's `Fs`
design (above) was kept as canonical** and the other session's two genuinely independent
pieces were redone on top of it, fresh:

- **`FsRequest::List { path: PathBuf }`** - lists a directory's immediate entries as
  `{name, path, is_dir, size}`. A directory's `size` is a **real recursive sum** of every file
  under it (`directory_size`, private to `effects.rs`), computed here so a caller (Flux) never
  needs list-iteration/recursion syntax it doesn't have to answer "how big is this folder."
  Same "narrow primitive, not a port" relationship to Zen's own `fs_list`/`fs_list_builtin`
  that `Read`/`Write` already have to `workspace_read`/`pipe_save` - not wired into `executor.rs`,
  Zen's own listing stays untouched.
- **`net.rs` / `Effect::Net(NetRequest)`** - a data shape only, no HTTP client, no performer
  here (this section's own reasoning for skipping `Net` entirely still applies: no generic `net`
  capability in Zen, and a real implementor needs `ureq`, which the dependency-footprint
  guardrail keeps out of `zen-runtime`). `flux-lang` supplies its own performer with its own
  `ureq` dependency instead.

`cargo test --workspace`: 279 (zen) + 22 (zen-runtime, up from 19), zero regressions.

## Phase 3.4 - a real, durable `Journal` implementor

Flux's Phase 3.4 (durable functions) needed to lower onto `Journal` for real - and reading the
*only* implementor, `WorkflowPersistence`, before doing that surfaced a real trait-shape gap:
its `append` is deliberately just an in-memory `HashMap` write (see its own doc comment); real
SQL durability happens through a separate, Zen-specific function (`persist_workflow_step`) that
bypasses `Journal` entirely. That's correct for Zen (`resume()` does one real SQL read at the
start of a run; nothing needs `append` itself to be durable mid-run), but it means the trait's
own documented contract ("durably append... before continuing") wasn't actually honored by
anything. Flux's durable functions genuinely need it to be - a step can be journaled between any
two lines of orchestration code, not just at Zen's fixed per-run resume point.

New `zen-runtime/src/durable_journal.rs`: `SqliteJournal`, additive alongside
`WorkflowPersistence` - doesn't touch it or any Zen code path. `append`/`suspend` do a real,
synchronous SQL `INSERT ... ON CONFLICT ... DO UPDATE` before returning (proven by a test that
appends, drops the connection entirely, reopens fresh, and confirms the row survived - not just
an in-memory assertion). New table, `durable_steps(instance_id, call_site, loop_key, status,
result_json, updated_at)`, own file/path (a real integration pointing this at the same
`.zen/runtime.db` Zen uses is a natural follow-on, not attempted here). Reuses
`values::{value_to_json, json_to_value}` (already existed) for `StepOutcome::Done(Value)`
serialization - no new dependency.

`cargo test --workspace`: 279 (zen) + 28 (zen-runtime, up from 22), zero regressions.

## Phase 3.4 follow-on - `deliver`, the `durable_instances` registry, and cross-process locking

Three Flux-driven slices, each requested by `flux-lang`'s own `PHASE3-PLAN.md` as Flux's durable-fn
work (`await`/suspension, then a system-wide dispatcher, then concurrent-replay safety) hit a real
gap in what `Journal` and `SqliteJournal` could do. All additive to `zen-runtime`; `WorkflowPersistence`
(Zen's own implementor) stubs every new method to a no-op/always-succeed default, since Zen has no
suspension-and-resume-from-a-dispatcher story and no concurrent-replay story of its own (a single
`WorkflowEngine` process). Landed as three merged branches: `flux/await-suspension`,
`flux/dispatcher`, `flux/instance-lock`.

**`Journal::deliver`** (`flux/await-suspension`, commit `c634621`) - `deliver(signal, value)` manually
delivers a value to whichever suspended step is waiting on `signal`, returning the `StepId` now marked
done (or `None` if nothing matches). Exists because Flux's new `await(<expr>)` needed a way to
durably record "this signal arrived" independent of a live process - a caller re-invokes the durable
fn separately afterward to actually resume past the `await`. `SqliteJournal::deliver` does a `SELECT`
for the exact `(instance_id, call_site, loop_key)` primary key first, then an `UPDATE` keyed by that
exact tuple - deliberately not a single predicate-based `UPDATE ... WHERE status='suspended' AND
result_json=?`, since SQLite's bundled build has no portable `LIMIT` on `UPDATE` and a predicate-only
form would silently mark every suspended row sharing that signal string as done in one call.

**`durable_instances` registry** (`flux/dispatcher`, commits `ec32532`/`d55e622`) - three new trait
methods plus a new table, letting a caller discover and resume *any* suspended instance in a journal
knowing nothing but its `InstanceId` (what Flux's `--dispatch` needed - no more `--durable`/`--instance`
required per invocation). `register_instance(instance, fn_name, args, source)`: idempotent
(`INSERT OR IGNORE` - first write wins; a caller re-registering the same instance with different
args/fn_name is treated as a caller bug, not an update), embedding the **full script source text**, not
a path, so resume always replays the exact text that produced existing journal rows even if the
`.flux`/source file is edited later. `list_incomplete_instances()`: every registered instance not yet
marked completed - across *all* instances in the journal, unlike `append`/`suspend`/`deliver`/`resume`,
which are scoped to one `InstanceId`. `lookup_instance(instance)`: returns what `register_instance`
recorded (`fn_name`/`args`/`source`), or `None` if never registered.

A real bug was caught before shipping, not just anticipated: the first version inferred an instance's
discoverability from `durable_steps.status = 'suspended'` - but `deliver` already flips a delivered
step's own row straight to `'done'`, completely independent of whether the durable fn's remaining
orchestration body has ever actually been re-run. A dispatcher relying on that signal would silently
and permanently stop discovering an instance the instant it was delivered, even though nobody had
resumed it to real completion. Fixed by tracking completion at the *instance* level instead of
inferring it from step state: new `mark_instance_completed(instance)`, called by the caller only once
resuming genuinely finishes the whole orchestration body - not merely once one `await` resolves. New
table, `durable_instances(instance_id, fn_name, args_json, source, created_at, completed_at)`, with
`completed_at` starting `NULL`. `list_incomplete_instances_still_includes_an_instance_after_delivery_until_marked_completed`
locks this in directly.

**Exclusive instance locking with PID-liveness auto-reclaim** (`flux/instance-lock`, commits
`cad211e`/`99acd88`) - closes a real, previously untested race in concurrent replay, confirmed by
reading the code rather than just inferred: a step's journal check runs against an **in-memory cache**
populated once at the start of a resume, so two processes racing to resume the same suspended instance
each see "not journaled yet" for the same step, both fire its real effects, and whichever appends
second silently clobbers the first's result with no error surfaced anywhere. Fixed with a new
`durable_instance_locks(instance_id, locked_at, pid)` table - the same "let SQLite's own row-uniqueness
be the coordination primitive" trick `register_instance`'s `INSERT OR IGNORE` already uses, now for
exclusivity instead of idempotency. New `Journal::try_lock_instance`/`unlock_instance`; the claim
(check-then-insert-or-update) runs inside one `rusqlite` transaction, making it atomic against a second
racing claim attempt - the same guarantee that already makes `append`'s upsert safe. **Auto-reclaimed
via OS PID-liveness** if the lock's holder has crashed (`sysinfo`, already a `zen-runtime` dependency,
so no new one needed) - confirmed as the right call over "fail-fast forever, no reclaim": a lock that
outlives its holder's crash forever would make crash-resilience *worse* than before this feature
existed. Stated, accepted risk: PID reuse could in rare cases steal a still-live process's lock (a
well-known limitation of PID-liveness checks) - single-machine only, matching this project's actual
deployment model. `unlock_instance` only deletes a lock row matching *both* the instance and the
calling process's own PID, so a process can never release a lock it doesn't hold.

Also set a `busy_timeout` (5000ms) on `SqliteJournal`'s connection - rusqlite defaults to 0ms, an
immediate `SQLITE_BUSY` error on any genuine write collision with no retry. A small, directly-related
robustness fix surfaced while building the concurrent-replay test itself: without it, a real collision
could bypass the lock's own clean conflict message with a raw SQLite error instead.

Proven end-to-end from the Flux side with a real two-thread race (not sequenced/simulated): two
threads, each with its own `SqliteJournal` connection, both attempt the same instance at the same
instant via a `Barrier` - exactly one completes, the other is rejected immediately with a clear error,
and the step's real effect fires exactly once, not twice.

`cargo test --workspace`: 279 (zen) + 45 (zen-runtime, up from 28), zero regressions.

## Current status

Every trait method `flux-lang`'s `PHASE3-PLAN.md` has needed from `zen-runtime` through its own
Tier 1 compilation, type-checking, and `flux-dba` work is already built and merged here - from that
doc's own "Current status" section onward, essentially every subsequent Flux slice explicitly notes
"no `zen-runtime` changes." There is no `zen-runtime` work presently queued from the Flux side; the
next real trigger for touching this crate again is a new Flux slice that needs something new from the
shared traits (e.g. crash-durable retry/rollback state, or a real multi-journal/event-bus dispatcher -
both named as permanent, deliberately-deferred design boundaries in `PHASE3-PLAN.md`, not scheduled
work).
