# Book Examples

Runnable examples for [The Zen Operations Book](../the-zen-operations-book.md).

Run script examples from the workspace root:

```powershell
zen run book\examples\01-hello.zen
zen run book\examples\02-permissions.zen --yes
```

Run workflow examples from the workspace root:

```powershell
zen run book\examples\08-release-workflow.yaml --yes
```

Check the examples without running service-changing workflows:

```powershell
powershell -ExecutionPolicy Bypass -File book\check-examples.ps1
```

Also run the harmless examples:

```powershell
powershell -ExecutionPolicy Bypass -File book\check-examples.ps1 -RunHarmless
```

Some examples touch local services or accounts:

- `05-postgres-auth.zen` checks local PostgreSQL access.
- `06-secrets.zen` reads Windows Credential Manager secret names.
- `07-dropbox-status.zen` checks Dropbox credentials.
- `09-postgres-backup-workflow.yaml` can create `backup.sql`.
- `10-backup-to-dropbox-workflow.yaml` can create `backup.sql` and upload it to Dropbox.

## Files

- [01-hello.zen](01-hello.zen): first script.
- [02-permissions.zen](02-permissions.zen): `requires` and workspace listing.
- [03-pipelines.zen](03-pipelines.zen): fields, JSON, and saving output.
- [04-files.zen](04-files.zen): copy a file and package a zip.
- [05-postgres-auth.zen](05-postgres-auth.zen): check passwordless PostgreSQL auth.
- [06-secrets.zen](06-secrets.zen): inspect saved secret names.
- [07-dropbox-status.zen](07-dropbox-status.zen): check Dropbox auth status.
- [08-release-workflow.yaml](08-release-workflow.yaml): release zip workflow.
- [09-postgres-backup-workflow.yaml](09-postgres-backup-workflow.yaml): smart PostgreSQL backup.
- [10-backup-to-dropbox-workflow.yaml](10-backup-to-dropbox-workflow.yaml): backup then upload to Dropbox.
