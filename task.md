# Task: Protected workflow secrets for Zen YAML runs

## Goal

Allow Zen to run normal YAML workflows with protected secret input supplied by an external broker such as:

```powershell
gp-vault with postgres.dev -- zen run pg-backup.yaml --yes
```

The workflow command should stay `zen run <workflow.yaml>`. The secret mechanism should be an input channel/context, not a new Zen command.

Plaintext secret values must stay inside the trusted runtime path only. They may exist transiently in RAM while moving from the broker to Zen and from Zen to a trusted sink, but must never be written to disk or exposed through ordinary command, output, status, or logging surfaces.

## Problem

Zen already supports YAML workflows and saved secrets. Plugin calls such as Postgres and Dropbox can receive `Value::Secret` through call config, but workflow `run.env` currently accepts literal strings only. Generic workflow shell commands therefore do not have a clean way to receive protected secrets without putting values in YAML, logs, shell history, or persisted workflow state.

## Desired UX

External broker provides a protected context:

```powershell
gp-vault with postgres.dev -- zen run pg-backup.yaml --yes
```

Workflow YAML references secret values symbolically:

```yaml
name: pg-backup
requires:
  - proc.exec

steps:
  - name: dump
    run: pg_dump postgres -f backup.sql
    env:
      PGPASSWORD:
        secret: postgres.password
```

The secret value is available only during execution and is unwrapped only for trusted sinks such as process environment or plugin environment.

## Design Principles

- Keep `zen run` as the normal workflow entry point.
- Treat secret input as a protected runtime binding, not ordinary workflow data.
- Keep plaintext secret values RAM-only and transient.
- Do not persist secret values in `.zen/runtime.db`, workflow events, outputs, artifacts, or `run-status`.
- Redact secret values everywhere they can be displayed.
- Never place secret values in command-line arguments or shell command strings.
- Prefer explicit object syntax like `{ secret: "postgres.password" }` over interpolation strings.
- Only trusted sinks may unwrap `Value::Secret`, such as process environment, process stdin, or plugin call config.

## Implementation Notes

Potential model:

- Add a workflow env value type that can represent either a literal string or a secret reference.
- Parse YAML step env values like:

```yaml
env:
  PGPASSWORD:
    secret: postgres.password
```

- Resolve secret references at run time from a protected input source.
- Pass resolved values into `ExecRequest.env` without exposing them through normal echo/string conversion.
- Persist only the symbolic secret name, never the secret value.

Possible protected input sources:

- Process environment injected by `gp-vault`.
- A broker-provided descriptor or handle.
- Existing Zen secret store as a fallback, if appropriate.

When process environment is used as the broker transport, Zen must treat those values as protected input and avoid copying them into persisted workflow state, logs, events, or printable outputs.

## Codex Implementation Guidance

Relevant areas to inspect first:

- `src/cli.rs` for `zen run` YAML loading and command-line flags.
- `src/runtime/executor.rs` for workflow parsing, env validation, workflow persistence, and `ExecRequest` construction.
- `src/runtime/values.rs` for `Value::Secret` behavior.
- `src/runtime/plugins/postgres.rs` and `src/runtime/plugins/dropbox.rs` for existing trusted handling of `Value::Secret`.
- `src/runtime/plugins/process.rs` for generic `exec` behavior.

Implementation should avoid real credentials. Use a unique canary string in tests, for example:

```text
ZEN_CANARY_SECRET_plaintext_must_not_persist
```

Do not print the canary except inside intentional negative test setup. Prefer assertions that inspect returned workflow values, persisted run rows, event payloads, and known runtime files for absence of the canary.

## Threat Model Boundary

In scope:

- Prevent plaintext secrets from being written to disk by Zen.
- Prevent plaintext secrets from appearing in Zen logs, status, events, outputs, artifacts, and command strings.
- Pass plaintext only through trusted runtime paths such as process env, stdin, or plugin env.

Out of scope for the first implementation:

- Proving the secret is absent from RAM.
- Preventing a deliberately malicious child process from printing or writing its received secret.
- OS-level memory forensics.
- Secret zeroization across all Rust string allocations.

## Acceptance Criteria

- `zen run pg-backup.yaml --yes` can pass a protected password into a workflow shell step env var.
- The password does not appear in stdout, stderr summaries, workflow status, events, artifacts, or persisted run records.
- A canary plaintext secret does not appear in the workspace, `.zen`, `%TEMP%`, or Zen config/runtime directories after a workflow run.
- YAML validation rejects malformed secret env entries.
- Literal string env values keep working unchanged.
- Existing `.zen` script secret behavior remains unchanged.
- Tests cover literal env, secret env, redaction, and persisted workflow state.
