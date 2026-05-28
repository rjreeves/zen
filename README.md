# Zen

Zen is a Windows-first scripting CLI with typed pipelines, explicit permissions, and small builtins for process execution, time, measuring, and delays.

For an outcome-first guide, see [The Zen Operations Book](book/the-zen-operations-book.md). For practical workflow examples, see [The Zen Automation Cookbook](book/the-zen-automation-cookbook.md) and [Book Examples](book/examples/README.md).

## Running Scripts

```powershell
cargo run -- time.now
cargo run -- pg.dump main
cargo run -- run script.fg
cargo run -- run script.fg --yes
cargo run -- explain script.fg
```

Unknown top-level commands are treated as one-off Zen input. For example, `zen pg.dump main` executes the same Zen line you would type in the REPL:

```fg
pg.dump main
```

One-off input can auto-approve declared permissions with `--yes`:

```powershell
zen "requires { dropbox.read } dropbox.status" --yes
zen 'requires { fs.read } archive.list "dist/zen-win64.zip"' --yes
```

Declare the permissions inside the one-off Zen input; `--yes` approves those declared permissions. Quote path arguments inside one-off input when they contain separators or punctuation so Zen parses them as string arguments.

## REPL

Start an interactive Zen session:

```powershell
cargo run -- repl
cargo run -- repl --yes
```

REPL commands:

```text
:help
:help keys
:help startup
:help :save PATH
:commands
:vars
:plugins
:permissions
:doctor
:reset
:history
:startup
:reload
:load examples/time.fg
:save scratch.fg
:clear
:quit
```

Short aliases are available for common session commands:

```text
:p -> :permissions
:s -> :startup
:r -> :reload
:d -> :doctor
```

Use `:plugins` to show loaded plugins and their commands. Use `plugins.list` when you want structured plugin metadata in a pipeline.

REPL keyboard shortcuts:

```text
Tab          Complete commands, paths, and variables
Enter        Submit complete input
Enter        Continue multiline input when braces are open
Alt+Enter    Insert an explicit newline
Up/Down      Browse history
Left/Right   Move the cursor
Ctrl+A       Move to start of line
Ctrl+E       Move to end of line
Ctrl+U       Clear before the cursor
Ctrl+K       Clear after the cursor
Ctrl+W       Delete the previous word
Ctrl+C       Cancel the current edit and return to the prompt
Ctrl+D       Exit the REPL
Ctrl+R       Search history
```

Tab completion covers REPL commands, loaded Zen/plugin command names, paths after `:load` and `:save`, and session variables after `$`.

External command plugins are loaded from `.zen\plugins\<plugin-name>\plugin.toml`:

```toml
name = "hello"
description = "A tiny external plugin that replies with hello."
version = "0.1.0"
author = "Zen workspace"
homepage = "https://example.com/hello"

[[commands]]
name = "hello.world"
run = "echo hello"
summary = "Prints hello."
usage = "hello.world"
examples = ["hello.world"]
```

External commands run through Zen's process executor, so they require `proc.exec`:

```fg
requires {
  proc.exec
}

hello.world
```

Arguments passed to the external Zen command are appended to the manifest `run` command. The command will appear in `:plugins`, `plugins.list`, and `help hello.world`.

External plugins can also be loaded, unloaded, or rediscovered during a session:

```fg
plugins.discover
plugins.load ".zen/plugins/hello"
plugins.unload hello
plugins.reload
```

Use `fields` in pipelines to keep only selected object fields:

```fg
workspace.files | fields name, path | echo table
workspace.find "*.rs" | fields path, size | echo table
plugins.list | fields name, kind, command_count | echo table
plugins.list | where kind == "external" | fields name, commands, source | echo table
```

Common data commands:

```fg
exec cargo test | get stdout
exec cargo test | pick stdout, exitcode, success
exec cargo test | where success == true
plugins.list | where kind == "external" | table
plugins.list | to-json | save result.json
workspace.read "result.json" | from-json | table
```

`pick` is an alias for strict field filtering. `from-json` parses JSON text, `to-json` converts structured data to JSON text, `table` displays pipeline input as a table, and `save` writes pipeline input to a workspace file. `save` requires `fs.write`.

Scheduler JSON examples:

```fg
scheduler.tasks.json | get stdout | from-json | table
scheduler.tasks.json | get stdout | save ".zen/schedules.json"
scheduler.store
workspace.read ".zen/schedules.json" | from-json | table
```

`scheduler.tasks.json` stores results as `{ success, exit_code, error, tasks }`, so scheduler query failures are still valid JSON.

On startup, the REPL runs startup files when they exist:

```text
<config-dir>\zen\startup.fg
.zen\startup.fg
```

Use startup files for session setup such as `let` variables and `requires` blocks.
Use `:startup` inside the REPL to show whether each startup file was loaded, missing, or failed.
Use `:reload` to re-run startup files without restarting the REPL.

REPL variables persist for the session:

```fg
let now = time.now
echo now
time.since "2026-05-01" | select days, human
```

Lists and objects can be assigned directly:

```fg
let tags = ["cli", "rust"]
let user = { name: "zen", count: 3, active: true, tags: tags }
let meta = {
  "display-name": "Zen",
  missing: null
}
```

Branch on boolean expressions with `if` and optional `else` blocks:

```fg
let status = dropbox.status

if status.configured == true && status.auth == "ok" {
  let dropbox_state = "ready"
} else {
  let dropbox_state = "needs-auth"
}

echo dropbox_state
```

Conditions must evaluate to booleans. Assignments made inside the selected branch are available after the block.

Shell-like builtins:

```fg
pwd
cd src
which clear
clear
```

`clear` and the REPL command `:clear` both clear the terminal screen. If multiline input is pending, `:clear` also discards that pending input. `cd` updates the Zen session working directory. `exec` uses that directory by default unless an explicit `workdir` is provided.

Declare permissions inside the session before privileged calls:

```fg
requires {
  proc.exec
}

exec cargo --version
```

Use `:permissions` inside the REPL to show permissions granted to the current session. `:perms` is a short alias.
Permission errors include a concise `Try: requires { ... }` suggestion.
Use `:doctor` to show a compact session summary, and `:reset` to clear variables and permissions before re-running startup files.

Scripts that execute processes must declare permission:

```fg
requires {
  proc.exec
}

exec cargo test
```

## Time

UTC timestamps:

```fg
time.now
time.timestamp
time.unix
time.millis
```

Local time:

```fg
time.local
time.local.format "%Y-%m-%d %H:%M"
time.local.stamp seen_at
```

Deterministic script time:

```fg
time.freeze "2026-05-14T12:34:56Z"
time.now
time.freeze clear
```

Parsing and differences:

```fg
time.parse "tomorrow"
time.parse "in 3 days"
time.parse "2 hours ago"

time.since "2026-05-01" | select days, human
time.until "2026-06-01T00:00:00Z" | select days, human
```

`time.parse`, `time.since`, and `time.until` accept RFC3339 timestamps, `YYYY-MM-DD` dates, epoch seconds, and supported phrases: `now`, `today`, `tomorrow`, `yesterday`, `in N unit`, and `N unit ago`.

## Measure

`measure` wraps one builtin call and returns timing metadata plus the wrapped result.

```fg
measure time.now
measure sleep 20ms | select duration_ms, result
measure exec cargo test
```

Output fields:

```text
success
duration_ms
started_at
ended_at
result
```

Elapsed duration uses monotonic time. `started_at` and `ended_at` use script time, so `time.freeze` makes labels deterministic but does not freeze elapsed duration.

## Benchmark

`benchmark` runs one builtin call repeatedly and returns timing statistics.

```fg
benchmark 10 sleep 20ms | select runs, min_ms, avg_ms, median_ms, max_ms
benchmark 5 time.now | select runs, avg_ms, failures
```

Output fields:

```text
runs
failures
success
min_ms
avg_ms
median_ms
max_ms
last_result
```

Benchmark durations use monotonic time. The wrapped call is executed once per run, and `last_result` contains the final successful result.

## Sleep

Pause execution:

```fg
sleep 500ms
sleep 2s
sleep 1m
sleep 5
```

Pipeline input passes through unchanged:

```fg
"ready" | sleep 500ms | echo
```

Sleep until a wall-clock time:

```fg
sleep until "2026-05-14T10:30:00Z"
sleep until "tomorrow"
```

Add jitter:

```fg
sleep jitter 500ms
sleep 2s jitter 250ms
```

`sleep` uses real elapsed/wall-clock time and is not affected by `time.freeze`.

## Exec

Run commands:

```fg
requires {
  proc.exec
}

exec cargo test
exec cargo test timeout 30s
exec cargo test retry 3 timeout 30s
```

Run from a working directory:

```fg
exec cargo test workdir "."
exec npm install workdir "frontend"
exec cd workdir src | select stdout, success
```

Wait for child processes:

```fg
exec npm run dev timeout 30s wait children
measure exec powershell -NoProfile -Command Start-Sleep -Milliseconds 500
```

`exec` returns an object with fields such as `success`, `status`, `exitcode`, `attempts`, `timed_out`, `stdout`, and `stderr`.

## Files

```fg
requires {
  fs.read
  fs.write
}

fs.copy "target/release/zen.exe" "dist/zen.exe"
fs.list "src" | fs.copy "backup"
```

`fs.copy` returns structured copy results with `success`, `source`, `destination`, and `bytes` for direct file copies.

## Archives

```fg
requires {
  fs.read
  fs.write
}

archive.zip "dist/zen-win64.zip" "dist/zen.exe" "dist/README.md" ".zen" exclude ".zen/runtime.db" ".zen/state.json" ".zen/schedules.json"
archive.list "dist/zen-win64.zip" | echo table
```

`archive.zip` creates a zip from workspace files or directories and returns `success`, `path`, `entries`, and `skipped`. Use `exclude` followed by paths to omit local runtime state or other files. Locked, unreadable, or excluded files are reported in `skipped` rather than crashing the whole archive. `archive.list` returns zip entries with `name`, `size`, `compressed_size`, and `directory`.

## Workflows

`zen run workflow.yaml` executes a YAML workflow as recoverable operational intent. Zen stores run state in `.zen/runtime.db` and logs workflow events. Use `zen resume <run-id>` to resume an incomplete run by skipping previously succeeded checkpointed steps.

```yaml
name: postgres-backup
requires:
  - postgres.read
  - proc.exec

steps:
  - name: check-auth
    zen: pg.auth.passwordless mydb
    save_as: pg_auth

  - name: dump-db
    if: outputs.pg_auth.passwordless == true
    run: pg_dump mydb > backup.sql
    save_as: dump
    artifacts:
      - name: backup_sql
        path: backup.sql
    timeout: 5m
    env:
      PGPASSWORD: secret-value
    retry:
      attempts: 3
      delay: 10s
    on_failure:
      - emit: backup.dump.failed
      - run: notify-admin
    rollback:
      - run: rm backup.sql
      - emit: backup.dump.rolled_back
    finally:
      - run: cleanup-temp-files

  - name: upload-dropbox
    if: outputs.dump.success == true
    run: zen dropbox upload backup.sql
    checkpoint: uploaded
```

```powershell
zen run workflow.yaml --yes
zen runs
zen run-status <run-id>
zen resume <run-id> --yes
```

See [examples/postgres-smart-backup.yaml](examples/postgres-smart-backup.yaml) for a workflow that checks passwordless PostgreSQL auth with an in-process `zen:` step, branches with step `if`, dumps `postgres` to `backup.sql`, verifies the file, and rolls back the backup if verification fails.
See [Scripts/zen-release-build.yaml](Scripts/zen-release-build.yaml) for a beta/pre-release workflow and [Scripts/zen-release-build-final.yaml](Scripts/zen-release-build-final.yaml) for the final release workflow. Both use `Scripts/bump-version.ps1`, build Zen in release mode, copy `target/release/zen.exe` and `README.md` into `dist` with `fs.copy`, create `dist/zen-win64.zip` with `archive.zip`, and verify it with `archive.list`.

Initial supported fields are `name`, top-level `requires`, `steps`, step `run` or `zen`, `if`, `save_as`, `artifacts`, `timeout`, `env`, `checkpoint`, `retry.attempts`, `retry.delay`, `on_failure`, `rollback`, and `finally`. Workflow `requires` is an optional list of permission strings such as `postgres.read`; Zen merges it with inferred `proc.exec` for `run` steps and inferred permissions for simple `zen` commands. Use `run` for shell commands and `zen` for in-process Zen/plugin commands. Step `if` supports paths like `outputs.dump.success`, operators `==` and `!=`, and string, boolean, number, or null literals. If the condition is false, Zen marks the step `skipped` and emits `step.skipped`. Step `artifacts` accepts a path string, a list of path strings, or objects like `{ name: release_zip, path: dist/zen-win64.zip }`; successful steps add a top-level `artifacts` summary with `step`, `path`, `absolute_path`, `exists`, `size`, and `directory`. Actions currently support `{ run: "..." }` and `{ emit: "event.name" }`. On step failure, Zen runs `on_failure`, rolls back the failed step, rolls back previously completed steps in reverse order, then runs `finally`. The `workflow.run` builtin remains available for in-session object workflows and returns structured step statuses, an `outputs` object keyed by `save_as` or step name, an `artifacts` summary, plus emitted events.

Use `zen runs` to list persisted workflow runs, `zen run-status <run-id>` to inspect steps and events, and `zen resume <run-id>` to resume the exact stored run using its original workflow source path.

## Release Checklist

```powershell
cargo test
Scripts\bump-version.ps1 show
cargo run -- run Scripts\zen-release-build.yaml --yes
cargo run -- run Scripts\zen-release-build-final.yaml --yes
pwsh -ExecutionPolicy Bypass -File $env:USERPROFILE\Desktop\forge\package-zen-dist.ps1
pwsh -ExecutionPolicy Bypass -File $env:USERPROFILE\Desktop\forge\dist\verify-zen-install.ps1
```

The beta release workflow bumps the patch version while preserving a prerelease suffix such as `-beta`. The final release workflow bumps the patch version, removes the prerelease suffix, then builds `dist/zen.exe`, `dist/README.md`, and `dist/zen-win64.zip`. The installer verifier should report `"valid": true` and pass the installed binary, examples, scripts, `.zen` plugin folder, and version checks.

Use [Scripts/bump-version.ps1](Scripts/bump-version.ps1) to change the Cargo package version intentionally:

```powershell
Scripts\bump-version.ps1 patch
Scripts\bump-version.ps1 minor
Scripts\bump-version.ps1 major
Scripts\bump-version.ps1 release
Scripts\bump-version.ps1 beta
Scripts\bump-version.ps1 set -Version 9.0.0
```

## PostgreSQL

PostgreSQL commands use the standard client tools (`psql`, `pg_dump`, and `pg_restore`) and return structured output with `success`, `status`, `exitcode`, `stdout`, and `stderr`.

```fg
requires {
  postgres.read
}

pg.version
pg.query main "select now()"
"select count(*) from users" | pg.query main
pg.auth.passwordless main
pg.dump main "backup.sql"
```

Restoring requires write permission:

```fg
requires {
  postgres.write
}

pg.restore main "backup.dump"
```

Pass client environment variables with call config:

```fg
pg.query "$DATABASE_URL" "select 1" { env: { PGPASSWORD: $pass } }
```

Check whether `psql` can connect without any password source:

```fg
pg.auth.passwordless postgres
```

`pg.auth.passwordless` forces `psql -w`, blanks `PGPASSWORD`, points `PGPASSFILE` at a non-existent file, sets `PGCONNECT_TIMEOUT` to 5 seconds by default, and returns `database`, `passwordless`, `success`, `reason`, `exitcode`, `stdout`, and `stderr`.

On Windows, PostgreSQL clients also read `%APPDATA%\postgresql\pgpass.conf`. Zen can create or update an entry:

```fg
requires {
  postgres.write
  secrets.read
}

let pass = secrets.get "postgres.fireworks.password"
pg.pass.path
pg.pass.set "localhost" "5432" "fireworks" "postgres" pass
```

`pg.pass.set` writes `host:port:database:user:password`, escaping `:` and `\` as required by PostgreSQL. Re-running it updates the matching host/port/database/user entry instead of appending a duplicate.

## Dropbox

Dropbox commands use the Dropbox HTTP API and need either a `DROPBOX_TOKEN` value, refresh-token credentials, or saved `secrets` values. Read calls require `dropbox.read`; writes require `dropbox.write`.

```fg
requires {
  dropbox.read
}

dropbox.list ""
dropbox.status
dropbox.account | select email
dropbox.list "/reports" | echo table
dropbox.metadata "/reports/q1.csv"
dropbox.download "/notes/todo.txt"
dropbox.download "/reports/q1.csv" "q1.csv"
dropbox.download.verify "/reports/q1.csv" "q1.csv"
dropbox.sync.plan.down "/reports" "reports"
dropbox.sync.down "/reports" "reports"
```

Local download paths are resolved under the Zen session working directory, and parent traversal such as `../outside.txt` is rejected.

Uploading and deleting require write permission:

```fg
requires {
  dropbox.write
}

dropbox.upload "q1.csv" "/reports/q1.csv"
dropbox.upload "q1.csv" "/reports/q1.csv" rename
dropbox.upload "q1.csv" "/reports/q1.csv" overwrite
dropbox.upload.verify "q1.csv" "/reports/q1.csv" overwrite
dropbox.sync.plan.up "reports" "/reports"
dropbox.sync.up "reports" "/reports"
dropbox.delete "/reports/old.csv"
```

Upload defaults to `add`, which refuses to overwrite an existing path. Use `rename` to keep both files with a Dropbox-generated name, or `overwrite` only when replacement is intentional.

Verify local file content against Dropbox metadata:

```fg
dropbox.hash "q1.csv" | select content_hash
let meta = dropbox.metadata "/reports/q1.csv"
dropbox.verify "q1.csv" meta.content_hash
dropbox.verify "q1.csv" "dropbox-content-hash-from-metadata"
```

Dropbox content hashes use Dropbox's block algorithm: SHA-256 each 4 MiB block, concatenate the block digests, then SHA-256 that concatenation.

Pass credentials with call config when you do not want to read them from the process environment:

```fg
dropbox.list "" { env: { DROPBOX_TOKEN: $token } }
dropbox.list "" { env: { DROPBOX_REFRESH_TOKEN: $refresh, DROPBOX_APP_KEY: $key, DROPBOX_APP_SECRET: $secret } }
```

Save Dropbox credentials once from call config or process environment:

```fg
requires {
  secrets.write
}

dropbox.secrets.save { env: { DROPBOX_REFRESH_TOKEN: $refresh, DROPBOX_APP_KEY: $key, DROPBOX_APP_SECRET: $secret } }
dropbox.secrets.save { env: { DROPBOX_TOKEN: $token } }
```

There is also a small C# helper that copies Dropbox environment variables directly into Windows Credential Manager using the same secret names:

```powershell
dotnet run --project examples/DropboxEnvToCredentialManager
```

It reads `DROPBOX_TOKEN`, or `DROPBOX_REFRESH_TOKEN` with `DROPBOX_APP_KEY`/`DROPBOX_CLIENT_ID`, plus optional `DROPBOX_APP_SECRET`/`DROPBOX_CLIENT_SECRET`.

Bootstrap Dropbox OAuth credentials from an app key:

```fg
requires {
  secrets.read
}

dropbox.auth.url { env: { DROPBOX_APP_KEY: $key } } | select url
dropbox.auth.finish $code { env: { DROPBOX_APP_KEY: $key, DROPBOX_APP_SECRET: $secret } }
dropbox.status
```

`dropbox.auth.url` asks Dropbox for an offline code flow. After you approve the app in a browser, pass the returned code to `dropbox.auth.finish`; it saves the refresh token and app credentials for future Dropbox commands.

Import a JSON secret bundle from Dropbox into Windows Credential Manager:

```fg
requires {
  dropbox.read
  secrets.write
}

dropbox.secrets.import "/zen/secrets.json"
```

Supported bundle shape:

```json
{
  "secrets": {
    "dropbox.refresh_token": "...",
    "dropbox.app_key": "...",
    "dropbox.app_secret": "..."
  }
}
```

## Secrets

Secrets are stored in Windows Credential Manager under `zen/default/<service>/<key>`. `secrets.set` prompts for the value without echoing it, and returned secret values render as `[secret]`.

```fg
requires {
  secrets.write
}

secrets.set "dropbox.refresh_token"
secrets.set "dropbox.app_key"
secrets.set "dropbox.app_secret"
```

Read or inspect saved secret names:

```fg
requires {
  secrets.read
}

secrets.exists "dropbox.refresh_token"
secrets.list | echo table
let refresh = secrets.get "dropbox.refresh_token"
echo refresh
```

Dropbox automatically checks `dropbox.refresh_token`, `dropbox.app_key`, and optional `dropbox.app_secret` when environment credentials are not present:

```fg
requires {
  dropbox.read
}

dropbox.list ""
```

`dropbox.list` follows Dropbox pagination automatically and returns one accumulated response.

## Pipelines

Use `select` to project fields from objects or lists:

```fg
measure time.now | select duration_ms, result
time.since "yesterday" | select hours, human
exec cd workdir src | select stdout, success
```
