# 10 Practical Zen Recipes

Zen is a Windows-first automation CLI for scripts and recoverable workflows.

This short cookbook is not a full language reference. It is a set of useful recipes you can run, inspect, and adapt.

## 1. Run A Script With Permissions

Goal: run a Zen script that declares what it needs before it does work.

```zen
requires {
  proc.exec
}

exec cargo test
```

Run it:

```powershell
zen run examples\exec.fg
```

Or skip the prompt when you already trust the script:

```powershell
zen run examples\exec.fg --yes
```

What happened: Zen read the `requires` block, asked for approval, then allowed the script to use process execution.

## 2. Copy A File Safely

Goal: copy one file inside the workspace and get structured output.

```zen
requires {
  fs.read
  fs.write
}

fs.copy "target/release/zen.exe" "dist/zen.exe"
```

Expected output includes:

```json
{
  "success": true,
  "source": "...target/release/zen.exe",
  "destination": "...dist/zen.exe",
  "bytes": 7515136
}
```

What happened: `fs.copy` resolved both paths inside the workspace, created the destination directory if needed, copied the file, and returned copy metadata.

## 3. Build A Release Zip

Goal: build Zen in release mode and package a distributable zip.

Use the checked-in workflow:

```powershell
zen run Scripts\zen-release-build.yaml --yes
```

For a final non-beta release:

```powershell
zen run Scripts\zen-release-build-final.yaml --yes
```

The workflow:

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
```

The full workflow also copies `README.md`, creates `dist\zen-win64.zip`, and verifies that the zip contains `zen.exe`, `README.md`, and `.zen`.

The archive steps use Zen-native commands:

```yaml
- name: zip-release
  if: outputs.readme.success == true
  zen: archive.zip "dist/zen-win64.zip" "dist/zen.exe" "dist/README.md" ".zen" exclude ".zen/runtime.db" ".zen/state.json" ".zen/schedules.json"
  save_as: zip
  artifacts:
    - name: release_zip
      path: dist/zen-win64.zip

- name: verify-release-zip
  if: outputs.zip.success == true
  zen: archive.list "dist/zen-win64.zip"
  save_as: zip_entries
```

What happened: this workflow mixed shell execution (`run`) with in-process Zen commands (`zen`), then used checkpoints to make each completed phase resumable. The `artifacts` fields make the final run output show the release files Zen produced, including their resolved paths and sizes.

## 4. Check PostgreSQL Passwordless Auth

Goal: check whether `psql` can connect without using a password source.

Run:

```powershell
zen run examples\check-psql-passwordless.zen --yes
```

Script:

```zen
requires {
  postgres.read
}

let database = "postgres"
let result = pg.auth.passwordless database

if result.passwordless {
  echo "psql can connect without a password"
  result | select database, reason, stdout, exitcode
} else {
  echo "psql cannot connect without a password"
  result | select database, reason, stderr, exitcode
}
```

What happened: `pg.auth.passwordless` forced `psql -w`, blanked `PGPASSWORD`, ignored the normal pgpass file, and returned a structured result.

## 5. Run A Smart PostgreSQL Backup

Goal: only run `pg_dump` if passwordless PostgreSQL auth works.

Run:

```powershell
zen run examples\postgres-smart-backup.yaml --yes
```

Key steps:

```yaml
- name: check-passwordless-auth
  zen: "pg.auth.passwordless postgres"
  save_as: pg_auth
  checkpoint: auth-checked

- name: dump-db
  if: outputs.pg_auth.passwordless == true
  run: "pg_dump postgres --file backup.sql"
  save_as: dump
  retry:
    attempts: 3
    delay: 10s
```

What happened: the first step captured structured auth output as `outputs.pg_auth`. The dump step only ran when `outputs.pg_auth.passwordless == true`.

## 6. Use Step Conditions

Goal: branch workflow execution without turning the workflow into a full programming language.

```yaml
- name: explain-auth-required
  if: outputs.pg_auth.passwordless == false
  run: "echo Postgres needs passwordless auth before backup can run."
```

Supported condition shape:

```text
outputs.some_key.some_field == true
outputs.some_key.some_field != "value"
```

Supported literals:

- strings
- booleans
- numbers
- null

What happened: when an `if` condition is false, Zen marks the step `skipped` and emits `step.skipped`.

## 7. Capture Outputs Between Steps

Goal: make one step's result available to later steps.

```yaml
- name: check-auth
  zen: "pg.auth.passwordless postgres"
  save_as: pg_auth

- name: continue-if-ready
  if: outputs.pg_auth.passwordless == true
  run: echo ready
```

If `save_as` is omitted, Zen uses the step name as the output key.

What happened: Zen collected step output in the workflow result:

```text
outputs.pg_auth.passwordless
```

## 8. Add Cleanup With Finally

Goal: always run cleanup or event emission after a step attempt finishes.

```yaml
- name: dump-db
  run: "pg_dump postgres --file backup.sql"
  finally:
    - emit: backup.dump.finished
```

What happened: `finally` actions run whether the main step succeeds or fails.

## 9. Roll Back On Failure

Goal: remove partial output when a later step fails.

```yaml
- name: verify-backup
  if: outputs.dump.success == true
  run: "powershell -NoProfile -Command \"if ((Test-Path -LiteralPath backup.sql) -and ((Get-Item -LiteralPath backup.sql).Length -gt 0)) { exit 0 } else { exit 1 }\""
  rollback:
    - run: "powershell -NoProfile -Command \"Remove-Item -LiteralPath backup.sql -ErrorAction SilentlyContinue\""
    - emit: backup.verify.rolled_back
```

What happened: when a step fails, Zen runs failure actions, rolls back the failed step, then rolls back prior completed steps in reverse order.

## 10. Resume A Workflow

Goal: continue a workflow after a failure without repeating checkpointed work.

Run a workflow:

```powershell
zen run examples\postgres-smart-backup.yaml --yes
```

List runs:

```powershell
zen runs
```

Inspect a run:

```powershell
zen run-status <run-id>
```

Resume it:

```powershell
zen resume <run-id> --yes
```

What happened: Zen stored workflow state in `.zen\runtime.db`. On resume, it skips steps whose checkpoints already succeeded and restores captured outputs for those skipped steps.

## Where To Go Next

After these recipes, the most useful next ideas are:

- add more workflow examples
- add richer status output for `zen run-status`
- build a small dashboard from the event log

Zen should stay focused on recoverable operational intent: state, events, checkpoints, rollback, and small composable commands.
