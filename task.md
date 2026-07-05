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
exactly) but leaves Phase 0's trait layer mostly unbuilt. This task backfills that layer,
staged as independent, behavior-preserving increments (per the plan's own working
discipline: "seams before boundaries," "working binary at every commit").

## Stages

| Stage | What | Status |
|---|---|---|
| 1 | `Capabilities` trait (`zen-runtime/src/capabilities.rs`) wrapping `PermissionSet`; `time`/`rand` added as auto-granted capability kinds | Done (commit `adbaa6d`) |
| 2 | `EventSink` trait (`zen-runtime/src/events.rs`); `WorkflowPersistence` implements it, `push_workflow_event`'s SQL write routes through `emit()` | Done (commit `91fdbcf`) |
| 3 | `Effects` trait (`zen-runtime/src/effects.rs`), scoped to the one effect kind already unified behind `exec_command`/`ExecRequest` (subprocess exec). Wired into the `exec` builtin, external-command syntax, the workflow engine, and `postgres.rs` | Done (commit `a8ec010`) |
| 4 | **`Journal` trait** - reshape `WorkflowPersistence` (bespoke SQL against `workflow_runs`/`workflow_steps`/`workflow_events`, tied to named steps) into `lookup`/`append`/`suspend`/`resume` per `DURABLE-EXECUTION.md`'s explicit mapping (named step → `StepId { call_site: step_name, loop_key: None }`, checkpoint → append, resume → replay). This is Zen-side work: implementing the *restricted client* of a trait shape designed Flux-first. Must preserve exact current behavior (rollback order, `finally`, output restoration) - the highest-risk, highest-value remaining stage. | Not started |
| 5 | `PluginHost` reshape (completes Phase 0.2) - narrow `PluginHost` to the 3-method capability shape (`use_capability`/`emit`/`secret`); move the ~50 `Expr`-based builtin dispatch methods it currently carries out of the runtime-facing trait. Touches every plugin file (`dropbox.rs`, `postgres.rs`, `fs.rs`, `state.rs`, `string.rs`, `time.rs`, `math.rs`, `workflow.rs`, `core.rs`, `archive.rs`, `external.rs`, `process.rs`, `secrets.rs`). Should come last, once Effects/Journal exist for it to route through. | Not started |

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
