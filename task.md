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
composition problems worth flagging back to Flux's design, rather than attempting the full
`executor.rs` sweep:**

1. **`RuntimeContext`'s separate-fields shape doesn't construct when one type backs
   multiple bundled traits.** `RUNTIME-INTERFACES.md`'s illustrative `RuntimeContext` holds
   five independent `&mut dyn Trait` fields (`caps`, `effects`, `journal`, `plugins`,
   `events`). For `Executor` - which implements `PluginHost`, `EventSink`, and
   `SecretStore` directly, while `Capabilities` is only reachable through its
   `permissions` field - holding `plugins: &mut dyn PluginHost` (a borrow of the whole
   `Executor`) and `caps: &mut dyn Capabilities` (a borrow of `self.permissions`, a field
   of that same `Executor`) at the same time is a real aliasing conflict, not a style
   choice. Same category of problem `WorkflowHost` already solved via supertraits instead
   of separate fields.
2. **Routing an existing concrete-module call through a shared trait isn't always small.**
   Attempted as a second proof point: `dropbox.rs`'s `secret_value()` and its callers
   (`access_token_from_env`, `credential_summary`, `app_key_from_env_or_secret`,
   `app_secret_from_env_or_secret`) are free functions several layers deep with no host
   parameter at all; routing them through `SecretStore::read_secret` would mean threading
   a host reference through all of them. Separately, `secrets.rs`'s own plugin functions
   (`secrets_get`, `secrets_exists`, `resolve_env_config`) already receive a host handle,
   but it's typed as the *existing*, wide `runtime::plugin::PluginHost` - which has no
   `read_secret` method and doesn't have `SecretStore` as a supertrait - so even
   same-crate call sites with a host handle in scope can't reach the shared trait without
   either widening that existing trait's bounds (avoided in Stage 5 deliberately) or adding
   a second parameter. Concluded this wasn't worth forcing for no behavior change; noted
   here instead of implemented.

**Deliberately deferred:** the full "every call site in `executor.rs` takes
`RuntimeContext`" rewrite. Given Zen has exactly one implementation of each trait today,
an exhaustive sweep wouldn't surface a *new* gap beyond the two above - the real test needs
a second caller (Flux) to be meaningful, matching the docs' own "design Flux-first,
validate with Zen" ordering.

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
