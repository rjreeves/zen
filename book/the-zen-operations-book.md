# The Zen Operations Book

Outcome-first automation with scripts, plugins, secrets, files, PostgreSQL, Dropbox, and recoverable workflows.

## What This Book Is For

This book is for people who want to use Zen to get operational work done.

It starts with outcomes instead of syntax. You will learn how to run small scripts, inspect results, protect dangerous actions with permissions, work with files, automate PostgreSQL, store secrets, move backups through Dropbox, and then join those pieces into recoverable workflows.

Zen's main idea is simple:

Shells execute commands. Zen executes recoverable operational intent.

That means a Zen automation should be inspectable, resumable, permission-aware, and honest about what it changed.

## Getting Started

### What Zen Is

Zen is a Windows-first scripting CLI and workflow runner. It has:

- explicit permissions with `requires`
- a REPL for exploration
- scripts for repeatable tasks
- plugins for structured commands
- typed pipeline values
- shell execution through `exec`
- YAML workflows with retry, rollback, checkpoints, resume, outputs, events, and artifacts

Zen is useful when a task is more than a one-liner but not large enough to deserve a full application.

### Why Permissions Exist

Permissions make power visible.

This script can run local processes:

```fg
requires {
  proc.exec
}

exec cargo test
```

This script can read and write files:

```fg
requires {
  fs.read
  fs.write
}

fs.copy "README.md" "dist/README.md"
```

This script can read secrets:

```fg
requires {
  secrets.read
}

let token = secrets.get "dropbox.access_token"
```

The goal is not bureaucracy. The goal is trust. You should be able to open a script and see what kind of authority it needs before it runs.

### Run Your First `.zen` Script

Create `hello.zen`:

```fg
echo "hello from Zen"
```

Run it:

```powershell
zen run hello.zen
```

Or run a checked-in example:

```powershell
zen run examples\time.fg
```

The extensions `.fg` and `.zen` are both used in this project. Treat them as Zen script files.

Book example: [01-hello.zen](examples/01-hello.zen).

### Understanding `requires`

Use `requires` when a script needs a capability:

```fg
requires {
  workspace.read
}

workspace.files "src" | echo table
```

Run with a prompt:

```powershell
zen run script.zen
```

Run with automatic approval when you trust it:

```powershell
zen run script.zen --yes
```

Book example: [02-permissions.zen](examples/02-permissions.zen).

### `run`, `repl`, `explain`, And `audit`

Run a script:

```powershell
zen run script.zen
```

Start an interactive session:

```powershell
zen repl
```

Explain a script before running it:

```powershell
zen explain script.zen
```

View the audit log:

```powershell
zen audit
```

The audit log records script runs. This matters once automation starts changing real systems.

## Everyday Scripts

### Echo Values

Use `echo` to print values:

```fg
echo "hello"
echo 42
echo true
```

Use table output for lists of objects:

```fg
workspace.files "src" | echo table
```

### Variables

Assign values with `let`:

```fg
let name = "Zen"
echo name
```

Use variables as command arguments:

```fg
let path = "README.md"
workspace.read path
```

### Pipelines

Pipelines pass structured values from one command to the next:

```fg
time.since "2026-05-01" | select days, human
```

The left side produces a value. The right side transforms or displays it.

### Selecting Fields

Use `select` to keep the fields you care about:

```fg
measure time.now | select duration_ms, result
exec cargo --version timeout 10s | select stdout, success
```

Use `fields` when filtering lists of objects:

```fg
workspace.files "src" | fields name, path, size | echo table
```

Book example: [03-pipelines.zen](examples/03-pipelines.zen).

### Parsing JSON

Turn JSON text into structured values:

```fg
requires {
  proc.exec
}

exec docker ps --format json | get stdout | from-json | echo table
```

Turn structured values back into JSON:

```fg
plugins.list | to-json
```

### Saving Output

Save pipeline output to a workspace file:

```fg
requires {
  fs.write
}

plugins.list | to-json | save "dist/plugins.json"
```

Use this pattern when a script generates a report, manifest, or handoff file.

### Running Shell Commands With `exec`

Use `exec` for external tools:

```fg
requires {
  proc.exec
}

exec cargo test
exec cargo test timeout 30s
exec cargo test retry 3 timeout 30s
```

Run from another directory:

```fg
exec npm install workdir "frontend"
```

`exec` returns structured output:

```fg
exec cargo --version | select success, exitcode, stdout, stderr
```

Use `exec` when the command is truly external. Use plugins when Zen has a structured command for the operation.

## Working With Files

### Listing Files

Read workspace files:

```fg
requires {
  workspace.read
}

workspace.files
workspace.files "src" | echo table
workspace.find "*.rs" | fields path, size | echo table
```

Workspace paths stay inside the workspace. Parent traversal is rejected.

### Copying Files With `fs.copy`

Copy one file:

```fg
requires {
  fs.read
  fs.write
}

fs.copy "target/release/zen.exe" "dist/zen.exe"
```

Expected output:

```json
{
  "success": true,
  "source": "...target/release/zen.exe",
  "destination": "...dist/zen.exe",
  "bytes": 8147968
}
```

Copy files from a pipeline:

```fg
requires {
  fs.read
  fs.write
}

fs.list "src" | fs.copy "backup/src"
```

### Saving Generated Output

Generate a manifest:

```fg
requires {
  workspace.read
  fs.write
}

workspace.files "src" | fields path, size | to-json | save "dist/source-manifest.json"
```

This is often better than printing text. A later workflow step can read the manifest as structured data.

### Building A Release Folder

Use a script for a simple folder build:

```fg
requires {
  proc.exec
  fs.read
  fs.write
}

exec cargo build --release
fs.copy "target/release/zen.exe" "dist/zen.exe"
fs.copy "README.md" "dist/README.md"
```

Use a workflow when you want checkpoints, verification, artifacts, or resume.

### Packaging A Zip

Create a zip:

```fg
requires {
  fs.read
  fs.write
}

archive.zip "dist/zen-win64.zip" "dist/zen.exe" "dist/README.md" ".zen" exclude ".zen/runtime.db" ".zen/state.json" ".zen/schedules.json"
archive.list "dist/zen-win64.zip" | echo table
```

Use exclusions for local runtime state, schedules, temp files, and secrets.

Book example: [04-files.zen](examples/04-files.zen).

## PostgreSQL Automation

### Checking `psql`

Check the installed PostgreSQL client:

```fg
pg.version
```

If `psql` is not on `PATH`, fix that before writing more automation.

### Passwordless Auth Checks

Check whether `psql` can connect without any password source:

```fg
requires {
  postgres.read
}

pg.auth.passwordless postgres
```

`pg.auth.passwordless` forces `psql -w`, clears `PGPASSWORD`, points `PGPASSFILE` at a non-existent file, and returns a structured result.

Book example: [05-postgres-auth.zen](examples/05-postgres-auth.zen).

Use it before backup workflows:

```yaml
- name: check-passwordless-auth
  zen: pg.auth.passwordless postgres
  save_as: pg_auth
  checkpoint: auth-checked
```

### Running Queries

Run SQL:

```fg
requires {
  postgres.read
}

pg.query postgres "select now()"
"select count(*) from pg_database" | pg.query postgres
```

Use call config when a client environment variable is needed:

```fg
let pass = secrets.get "postgres.local.password"
pg.query postgres "select 1" { env: { PGPASSWORD: $pass } }
```

### Dumps And Restores

Dump a database:

```fg
requires {
  postgres.read
}

pg.dump postgres "backup.sql"
```

Restore a dump:

```fg
requires {
  postgres.write
}

pg.restore postgres "backup.dump"
```

Prefer workflows for backup and restore operations so you can add verification, rollback, and artifacts.

### Creating `.pgpass`

On Windows, PostgreSQL clients read `%APPDATA%\postgresql\pgpass.conf`.

Create or update an entry:

```fg
requires {
  postgres.write
  secrets.read
}

let pass = secrets.get "postgres.fireworks.password"
pg.pass.path
pg.pass.set "localhost" "5432" "fireworks" "postgres" pass
```

Re-running `pg.pass.set` updates the matching host, port, database, and user entry instead of appending duplicates.

### Smart Backup Workflow

Use the checked-in workflow:

```powershell
zen run examples\postgres-smart-backup.yaml --yes
```

The shape is:

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
```

The outcome is not merely "run pg_dump". The outcome is "produce a backup only when auth is ready, retry temporary failures, record the artifact, and clean up partial work."

## Secrets

### Storing Secrets

Zen stores secrets in Windows Credential Manager under `zen/default/<service>/<key>`.

Store a secret:

```fg
requires {
  secrets.write
}

secrets.set "dropbox.refresh_token"
```

The prompt does not echo the value.

### Reading Secrets

Read a secret:

```fg
requires {
  secrets.read
}

let refresh = secrets.get "dropbox.refresh_token"
echo refresh
```

Secret values render as `[secret]` when printed.

List names without values:

```fg
requires {
  secrets.read
}

secrets.list | echo table
secrets.exists "dropbox.refresh_token"
```

Book example: [06-secrets.zen](examples/06-secrets.zen).

### Using Secrets In Call Config

Use secrets as environment variables for one command:

```fg
requires {
  secrets.read
  postgres.read
}

let pass = secrets.get "postgres.local.password"
pg.query postgres "select 1" { env: { PGPASSWORD: $pass } }
```

This avoids writing the secret into the script output.

### Avoiding Secrets In Shell History

Prefer:

```fg
let token = secrets.get "dropbox.access_token"
dropbox.list "" { env: { DROPBOX_TOKEN: $token } }
```

Avoid:

```powershell
zen "dropbox.list \"\" { env: { DROPBOX_TOKEN: \"actual-token-here\" } }"
```

Secrets typed into shell commands can end up in terminal history, process listings, logs, or screenshots.

## Dropbox

### Checking Auth Status

Check Dropbox readiness:

```fg
requires {
  dropbox.read
}

dropbox.status
dropbox.account | select email
```

Dropbox commands can use process environment variables, call config, or saved secrets.

Book example: [07-dropbox-status.zen](examples/07-dropbox-status.zen).

### Listing Files

List the root folder:

```fg
requires {
  dropbox.read
}

dropbox.list "" | echo table
dropbox.list "/reports" | echo table
dropbox.metadata "/reports/q1.csv"
```

`dropbox.list` follows pagination and returns accumulated results.

### Uploading Backups

Upload a file:

```fg
requires {
  dropbox.write
}

dropbox.upload "backup.sql" "/backups/backup.sql"
```

Choose upload mode intentionally:

```fg
dropbox.upload "backup.sql" "/backups/backup.sql" rename
dropbox.upload "backup.sql" "/backups/backup.sql" overwrite
```

The default upload mode is safe add. It refuses to overwrite an existing path.

### Sync Planning

Plan before changing remote state:

```fg
requires {
  dropbox.read
}

dropbox.sync.plan.up "reports" "/reports"
dropbox.sync.plan.down "/reports" "reports"
```

Run the sync only after the plan looks right:

```fg
requires {
  dropbox.write
}

dropbox.sync.up "reports" "/reports"
```

### Verified Uploads

Upload and verify content:

```fg
requires {
  dropbox.write
}

dropbox.upload.verify "backup.sql" "/backups/backup.sql" overwrite
```

Manual verification pattern:

```fg
dropbox.hash "backup.sql" | select content_hash
let meta = dropbox.metadata "/backups/backup.sql"
dropbox.verify "backup.sql" meta.content_hash
```

Use verified upload for backups and release artifacts.

## Workflows

### `zen run workflow.yaml`

Run a workflow:

```powershell
zen run workflow.yaml --yes
```

List and inspect runs:

```powershell
zen runs
zen run-status <run-id>
```

Resume:

```powershell
zen resume <run-id> --yes
```

Workflows store state in `.zen/runtime.db`.

### `run:` Vs `zen:`

Use `run:` for external commands:

```yaml
- name: build-release
  run: cargo build --release
```

Use `zen:` for in-process Zen/plugin commands:

```yaml
- name: copy-release
  zen: fs.copy "target/release/zen.exe" "dist/zen.exe"
```

Prefer `zen:` when it exists. It avoids shell quoting and returns structured values directly.

### `if`

Gate a step on earlier output:

```yaml
- name: dump-db
  if: outputs.pg_auth.passwordless == true
  run: pg_dump postgres --file backup.sql
```

Supported operators are `==` and `!=`.

If false, Zen marks the step `skipped` and emits `step.skipped`.

### `retry`

Retry a fragile operation:

```yaml
- name: upload
  run: zen dropbox upload backup.sql /backups/backup.sql
  retry:
    attempts: 3
    delay: 10s
```

Keep retry logic simple at first.

### `finally`

Always run cleanup:

```yaml
- name: dump-db
  run: pg_dump postgres --file backup.sql
  finally:
    - emit: backup.dump.finished
```

Use `finally` for cleanup, lock release, and final event emission.

### `rollback`

Undo partial work after failure:

```yaml
- name: create-backup
  run: pg_dump postgres --file backup.sql
  rollback:
    - run: powershell -NoProfile -Command "Remove-Item -LiteralPath backup.sql -ErrorAction SilentlyContinue"
    - emit: backup.removed
```

When a later step fails, Zen rolls back completed steps in reverse order.

### `checkpoint`

Mark completed work as resumable:

```yaml
- name: build-release
  run: cargo build --release
  checkpoint: release-built
```

On resume, Zen skips previously succeeded checkpointed steps and restores their outputs.

### `resume`

Resume a failed workflow:

```powershell
zen resume <run-id> --yes
```

Use `zen run-status <run-id>` first so you know what succeeded, failed, or skipped.

### `outputs`

Capture a step result:

```yaml
- name: check-auth
  zen: pg.auth.passwordless postgres
  save_as: pg_auth
```

Read it later:

```yaml
- name: continue
  if: outputs.pg_auth.passwordless == true
  run: echo ready
```

### `events`

Workflow transitions emit events:

```text
workflow.started
step.started
step.succeeded
step.retrying
step.failed
step.skipped
workflow.completed
workflow.failed
```

You can also emit custom events:

```yaml
finally:
  - emit: backup.dump.finished
```

Events are the bridge to monitoring, dashboards, agents, and later automation reactions.

## Real Recipes

### Build Zen Release Zip

Run:

```powershell
zen run Scripts\zen-release-build.yaml --yes
```

For a final non-beta release, run:

```powershell
zen run Scripts\zen-release-build-final.yaml --yes
```

Book example: [08-release-workflow.yaml](examples/08-release-workflow.yaml).

Outcome:

- builds `target/release/zen.exe`
- copies `zen.exe` and `README.md` into `dist`
- creates `dist/zen-win64.zip`
- includes `.zen` plugin assets
- excludes `.zen/runtime.db`, `.zen/state.json`, and `.zen/schedules.json`
- reports release artifacts

### Smart Postgres Backup

Run:

```powershell
zen run examples\postgres-smart-backup.yaml --yes
```

Book example: [09-postgres-backup-workflow.yaml](examples/09-postgres-backup-workflow.yaml).

Outcome:

- checks passwordless PostgreSQL auth
- skips backup when auth is not ready
- dumps only when safe
- retries temporary dump failure
- verifies output
- emits lifecycle events
- rolls back partial files on failure
- reports `backup.sql` as an artifact

### Backup Then Upload To Dropbox

Shape:

```yaml
name: postgres-backup-to-dropbox

requires:
  - postgres.read
  - proc.exec
  - dropbox.write

steps:
  - name: check-auth
    zen: pg.auth.passwordless postgres
    save_as: pg_auth
    checkpoint: auth-checked

  - name: dump-db
    if: outputs.pg_auth.passwordless == true
    run: pg_dump postgres --file backup.sql
    save_as: dump
    artifacts:
      - name: backup_sql
        path: backup.sql
    checkpoint: dumped

  - name: upload-dropbox
    if: outputs.dump.success == true
    zen: dropbox.upload.verify "backup.sql" "/backups/backup.sql" overwrite
    save_as: upload
    checkpoint: uploaded
```

Outcome: a database dump is produced locally and uploaded only after the dump succeeds.

Book example: [10-backup-to-dropbox-workflow.yaml](examples/10-backup-to-dropbox-workflow.yaml).

### Check A Service And Notify

Shape:

```yaml
name: service-check

requires:
  - proc.exec

steps:
  - name: check-service
    run: powershell -NoProfile -Command "Get-Service Spooler | ConvertTo-Json"
    save_as: service
    retry:
      attempts: 2
      delay: 5s
    on_failure:
      - emit: service.check.failed
      - run: echo Service check failed
```

Outcome: a service probe has retry and a failure signal.

### Scheduled Health Check

Zen already has workflow state and events. A scheduler can run:

```powershell
zen run health-check.yaml --yes
```

Workflow shape:

```yaml
name: health-check

steps:
  - name: check-time
    zen: time.now
    save_as: now

  - name: check-workspace
    zen: workspace.exists "README.md"
    save_as: readme_exists
```

Outcome: a small repeatable health check with structured outputs.

### Recover From Failed Backup

Use:

```powershell
zen runs
zen run-status <run-id>
zen resume <run-id> --yes
```

Outcome: checkpointed steps are skipped, outputs are restored, and the workflow continues from the failed area.

## Design Philosophy

### Scripts Execute Commands

A script is best when the task is short:

```fg
requires {
  workspace.read
}

workspace.files "src" | echo table
```

Scripts are good for exploration, reporting, and repeatable single-session tasks.

### Zen Executes Recoverable Operational Intent

A workflow is best when the task has state:

- Did the auth check pass?
- Did the dump finish?
- Was the zip created?
- Which files were produced?
- What failed?
- What should be retried?
- What should be rolled back?
- Can the run resume?

That is the difference between command execution and operational intent.

### Why Not Become A Full Programming Language

Zen should not rush into loops, complex expression systems, broad branching, or embedded application logic.

Those features are tempting, but they can blur the purpose of workflows.

The workflow should orchestrate:

- state
- events
- permissions
- outputs
- artifacts
- retries
- rollback
- resume

Application logic belongs in scripts, plugins, or external programs.

### Why Workflows Stay Small

Small workflows are easier to inspect and recover.

Prefer steps with names like:

- `check-auth`
- `dump-db`
- `verify-backup`
- `upload-dropbox`
- `cleanup-temp-files`

Avoid giant steps that hide multiple operations in one shell string.

When a workflow grows, split it by operational phase.

### Events, State, And Resumability

Events describe what happened. State describes where the workflow is. Checkpoints make completed work reusable. Artifacts show what the workflow produced.

Together, those make automation visible.

That is the Zen design center: not more syntax, but better recovery.
