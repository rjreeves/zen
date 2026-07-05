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
| 5 | New `PluginHost` trait (`zen-runtime/src/plugin_host.rs`) completing Phase 0.2 - the 3-method capability shape (`use_capability`/`emit`/`secret`), `EventSink`/`SecretStore` as supertraits. Auditing actual plugin call sites first revealed the original "narrow the existing `PluginHost`, touches every plugin file" framing was wrong: only 4 of its ~50 methods (`check_permission`, `plugin_arg_value`, `resolve_workspace_path`, `resolve_local_write_path`) are genuinely shared across diverse plugins; the other ~46 are each called by exactly one dedicated shim plugin proxying straight to `Executor`'s own interpreter-session state, with no Flux-shareable equivalent. So this stage added a small new trait alongside the existing one instead, additive like Stages 1-3 - zero plugin files touched, existing `runtime::plugin::PluginHost`/`ZenPlugin` untouched | Done (commit pending) |

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
