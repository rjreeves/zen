# The Zen Automation Cookbook

Practical recipes for recoverable operational workflows.

Zen is not trying to be another shell. Shells run commands. Zen runs operational intent: permissions, steps, outputs, events, checkpoints, artifacts, retries, cleanup, rollback, and resume.

This cookbook is written from that point of view. Each recipe is small enough to copy, but realistic enough to grow into something you can run every day.

## How To Read This Book

Use `.fg` or `.zen` scripts when you want a short interactive automation. Use YAML workflows when you need recoverability: retries, checkpoints, rollback, artifacts, and resumability.

Most examples assume you are running from the Zen workspace root.

```powershell
zen run path\to\script.fg
zen run path\to\workflow.yaml --yes
zen runs
zen run-status <run-id>
zen resume <run-id> --yes
```

## 1. Declare Permissions First

Goal: make automation explicit about the capabilities it needs.

```fg
requires {
  proc.exec
}

exec cargo test
```

Run it:

```powershell
zen run examples\exec.fg
```

What happened: Zen saw `proc.exec`, asked for approval, and only then allowed process execution.

This is the first Zen habit: do not hide power. Declare it.

## 2. Use In-Process Commands For Structured Work

Goal: copy a file without shell quoting problems and get structured metadata back.

```fg
requires {
  fs.read
  fs.write
}

fs.copy "target/release/zen.exe" "dist/zen.exe"
```

Expected shape:

```json
{
  "success": true,
  "source": "...target/release/zen.exe",
  "destination": "...dist/zen.exe",
  "bytes": 8147968
}
```

What happened: `fs.copy` stayed inside the workspace, created the destination directory, copied the file, and returned machine-readable output.

Prefer `zen:` workflow steps for this kind of work. Use `run:` when you really need an external process.

## 3. Build Your First Workflow

Goal: run a sequence of named operational steps.

```yaml
name: hello-workflow

steps:
  - name: say-hello
    run: echo hello

  - name: say-done
    run: echo done
```

Run it:

```powershell
zen run hello-workflow.yaml --yes
```

The output includes step status, events, and the final workflow status.

What happened: Zen treated the workflow as a state machine. Each step moved through `started` and `succeeded`, and the workflow emitted `workflow.completed`.

## 4. Capture Outputs Between Steps

Goal: let one step decide what another step should do.

```yaml
name: output-gate

steps:
  - name: check-auth
    zen: pg.auth.passwordless postgres
    save_as: pg_auth

  - name: continue-if-ready
    if: outputs.pg_auth.passwordless == true
    run: echo ready
```

What happened: `save_as: pg_auth` made the first step result available as `outputs.pg_auth`.

If `save_as` is omitted, Zen uses the step name as the output key.

## 5. Branch With Step Conditions

Goal: branch safely without turning workflows into a programming language.

```yaml
- name: explain-auth-required
  if: outputs.pg_auth.passwordless == false
  run: echo Postgres needs passwordless auth before backup can run.
```

Supported condition forms:

```text
outputs.some_key.some_field == true
outputs.some_key.some_field != "value"
```

Supported literals:

- strings
- booleans
- numbers
- null

What happened: if the expression is false, Zen marks the step `skipped` and emits `step.skipped`.

## 6. Add Retries For Fragile Operations

Goal: retry a step that may fail for temporary reasons.

```yaml
- name: dump-db
  run: pg_dump postgres --file backup.sql
  retry:
    attempts: 3
    delay: 10s
```

What happened: Zen reran the step up to three times. Each retry emitted `step.retrying`.

Keep retry policy boring at first. Fixed attempts and delay are enough for many operational workflows.

## 7. Always Clean Up With Finally

Goal: run cleanup whether the main step succeeds or fails.

```yaml
- name: dump-db
  run: pg_dump postgres --file backup.sql
  finally:
    - emit: backup.dump.finished
    - run: powershell -NoProfile -Command "Remove-Item -LiteralPath temp.dump -ErrorAction SilentlyContinue"
```

What happened: `finally` ran after the step attempt completed. This is where you put temporary file cleanup, lock release, or final event emission.

## 8. Roll Back Partial Work

Goal: undo previously completed work if a later step fails.

```yaml
name: rollback-example

steps:
  - name: create-backup
    run: pg_dump postgres --file backup.sql
    save_as: dump
    rollback:
      - run: powershell -NoProfile -Command "Remove-Item -LiteralPath backup.sql -ErrorAction SilentlyContinue"
      - emit: backup.removed

  - name: verify-backup
    if: outputs.dump.success == true
    run: powershell -NoProfile -Command "if ((Test-Path backup.sql) -and ((Get-Item backup.sql).Length -gt 0)) { exit 0 } else { exit 1 }"
```

What happened: if `verify-backup` fails, Zen rolls back completed steps in reverse order. Here, `create-backup` removes `backup.sql`.

Rollback is where workflows become operationally serious.

## 9. Save Checkpoints And Resume

Goal: continue after a failure without repeating completed work.

```yaml
name: resumable-build

steps:
  - name: build-release
    run: cargo build --release
    checkpoint: release-built

  - name: copy-release
    run: powershell -NoProfile -Command "Copy-Item target\release\zen.exe dist\zen.exe -Force"
    checkpoint: release-copied
```

Run, inspect, and resume:

```powershell
zen run resumable-build.yaml --yes
zen runs
zen run-status <run-id>
zen resume <run-id> --yes
```

What happened: Zen stored workflow state in `.zen/runtime.db`. On resume, checkpointed successful steps are skipped and their outputs are restored.

## 10. Produce Artifact Summaries

Goal: make the workflow result show the files it produced.

```yaml
- name: zip-release
  zen: archive.zip "dist/zen-win64.zip" "dist/zen.exe" "dist/README.md" ".zen" exclude ".zen/runtime.db" ".zen/state.json" ".zen/schedules.json"
  save_as: zip
  artifacts:
    - name: release_zip
      path: dist/zen-win64.zip
```

Workflow output includes:

```json
{
  "artifacts": [
    {
      "name": "release_zip",
      "step": "zip-release",
      "path": "dist/zen-win64.zip",
      "exists": true,
      "size": 4991435,
      "directory": false
    }
  ]
}
```

What happened: Zen inspected declared artifact paths after successful steps and added a top-level artifact summary.

Use this for release files, database dumps, reports, logs, and generated archives.

## 11. Package A Release Zip

Goal: build Zen and package a distributable archive.

Use the checked-in workflow:

```powershell
zen run Scripts\zen-release-build.yaml --yes
```

For a final non-beta release:

```powershell
zen run Scripts\zen-release-build-final.yaml --yes
```

Key steps:

```yaml
name: zen-release-build

requires:
  - proc.exec
  - fs.read
  - fs.write

steps:
  - name: build-release
    run: cargo build --release
    checkpoint: release-built

  - name: copy-release-to-dist
    if: outputs.build-release.success == true
    zen: fs.copy "target/release/zen.exe" "dist/zen.exe"
    save_as: copy
    artifacts:
      - name: zen_exe
        path: dist/zen.exe
    checkpoint: release-copied

  - name: zip-release
    if: outputs.copy.success == true
    zen: archive.zip "dist/zen-win64.zip" "dist/zen.exe" "dist/README.md" ".zen" exclude ".zen/runtime.db" ".zen/state.json" ".zen/schedules.json"
    save_as: zip
    artifacts:
      - name: release_zip
        path: dist/zen-win64.zip
    checkpoint: release-zipped
```

What happened: the workflow combined external build execution, in-process file copy, archive creation, archive exclusions, checkpoints, and artifact summaries.

This is the current best example of Zen's automation style.

## 12. Build A Smart PostgreSQL Backup

Goal: check auth, dump only when safe, verify the backup, and roll back bad output.

Use the checked-in workflow:

```powershell
zen run examples\postgres-smart-backup.yaml --yes
```

Core shape:

```yaml
name: postgres-smart-backup

requires:
  - postgres.read
  - proc.exec

steps:
  - name: check-passwordless-auth
    zen: pg.auth.passwordless postgres
    save_as: pg_auth
    checkpoint: auth-checked

  - name: explain-auth-required
    if: outputs.pg_auth.passwordless == false
    run: echo Postgres passwordless auth is not ready.

  - name: dump-db
    if: outputs.pg_auth.passwordless == true
    run: pg_dump postgres --file backup.sql
    save_as: dump
    artifacts:
      - name: backup_sql
        path: backup.sql
    retry:
      attempts: 3
      delay: 10s
    rollback:
      - run: powershell -NoProfile -Command "Remove-Item -LiteralPath backup.sql -ErrorAction SilentlyContinue"
    finally:
      - emit: backup.dump.finished
```

What happened: Zen used a structured plugin command to check auth, a step condition to branch, retries for the dump, and rollback to remove partial output.

This is the model for operational intent.

## 13. Inspect The Runtime

Goal: see what happened after the run.

```powershell
zen runs
zen run-status <run-id>
```

Look for:

- workflow status
- step status
- attempts
- checkpoints
- events
- errors

What happened: Zen stored runtime state in SQLite at `.zen/runtime.db`.

The runtime database is not release payload. Exclude it when packaging `.zen`.

## 14. Keep Archives Clean

Goal: include useful `.zen` assets but exclude local runtime state.

```fg
requires {
  fs.read
  fs.write
}

archive.zip "dist/zen-win64.zip" "dist/zen.exe" "dist/README.md" ".zen" exclude ".zen/runtime.db" ".zen/state.json" ".zen/schedules.json"
archive.list "dist/zen-win64.zip" | echo table
```

What happened: `archive.zip` included files and plugin assets, while excluded local runtime files were reported in `skipped`.

Use exclusions for:

- runtime databases
- local state
- schedules
- secrets
- temp files

## 15. A Pattern For New Workflows

When you create a new workflow, start with this skeleton:

```yaml
name: useful-name

requires:
  - proc.exec

steps:
  - name: probe
    run: echo checking
    save_as: probe
    checkpoint: probed

  - name: do-work
    if: outputs.probe.success == true
    run: echo working
    save_as: work
    retry:
      attempts: 3
      delay: 5s
    artifacts:
      - name: result
        path: dist/result.txt
    rollback:
      - run: powershell -NoProfile -Command "Remove-Item -LiteralPath dist\result.txt -ErrorAction SilentlyContinue"
    finally:
      - emit: work.finished
```

Then remove what you do not need.

## Design Rules

Use Zen workflows for operations that benefit from state. A good workflow has clear step names, explicit permissions, small steps, structured outputs, and recoverable checkpoints.

Avoid turning workflows into applications. Keep business logic in commands, plugins, or scripts. Let the workflow orchestrate state, recovery, and visibility.

Prefer:

- `zen:` for structured plugin commands
- `run:` for external tools
- `save_as` for outputs used later
- `if` for simple step gates
- `checkpoint` before expensive or irreversible work
- `artifacts` for files humans care about
- `rollback` for partial output
- `finally` for cleanup and events

Avoid:

- giant shell strings when a Zen/plugin command exists
- hidden permissions
- broad conditional systems too early
- loops before the state model needs them
- putting secrets in workflow files

## The Zen Way

Most automation fails because it is treated as a line of commands. Zen's bet is that automation should be treated as recoverable intent.

A strong Zen workflow answers five questions:

1. What permissions does this need?
2. What state is each step in?
3. What output should later steps depend on?
4. What files did this produce?
5. How do we recover if something fails?

That is the cookbook in one sentence: build automations you can inspect, resume, and trust.
