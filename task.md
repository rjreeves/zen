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

## Phase 3.0 - prerequisite runtime work for Flux (Flux-driven, from the Flux repo)

Flux's `docs/PHASE3-PLAN.md` sub-phase 3.0 blocks Flux's 3.2 (effects & capabilities): Flux's
`fetch`/`read`/`write` builtins need `Fs`/`Net` effects that didn't exist in `zen-runtime` -
`effects::Effect` had only `Process`. Landed the `Fs` half:

- New `zen-runtime/src/fs.rs`: `fs_read`/`fs_write`, a generic whole-file-as-text primitive.
  Deliberately narrow and new, not a port - Zen's own `fs.list`/`fs.copy`/`workspace.read`/
  `pipe_save` builtins each carry their own path-resolution (workspace-relative sandboxing)
  and capability-namespace rules; collapsing them into one generic primitive risked changing
  behavior neither this task nor Flux's needs require touching. Path resolution and the
  `fs.read`/`fs.write` permission check stay the caller's job, same division `process.rs`
  already established for `exec_command`.
- `effects::Effect` gained an `Fs(FsEffect)` variant (`FsEffect::Read`/`Write`); new
  `FsEffects` performer alongside the existing `ProcessEffects`, each erroring (not
  panicking) on the other's effect kind rather than one performer growing an exhaustive
  match over unrelated effect domains.
- **Not** wired into any existing Zen builtin or `executor.rs` call site - none of Zen's
  current fs commands have the plain "read/write this exact path" shape this primitive
  offers, so there was no existing call site to route through it without redesigning one of
  those commands, which is out of scope here. Proved against the real filesystem instead (no
  mocking): `zen-runtime/src/fs.rs` and `effects.rs` both gained tests that write and read
  real temp files.
- `Net`/`Db` remain unstarted. `Net` has no existing generic primitive to build from (only
  Dropbox-specific `ureq` calls in `dropbox.rs`) - a real design task (request/response
  shape, capability granularity, timeouts/redirects), not a port. `Db` is entangled with
  Flux's 3.3 (database pushdown) and likely needs shaping alongside that work, not ahead of
  it.

`cargo build --workspace` and `cargo test --workspace` both green: 279 (zen) + 20 (zen-runtime,
up from 15) passed, zero regressions.
