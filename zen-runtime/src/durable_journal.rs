//! A real, durably-persisting `Journal` implementor for Flux's durable
//! functions (`docs/DURABLE-EXECUTION.md` in the Flux repo). Additive
//! alongside `workflow.rs`'s `WorkflowPersistence` - doesn't touch it or
//! any Zen code path.
//!
//! Exists because `WorkflowPersistence::append` is deliberately only an
//! in-memory cache write (see its own doc comment) - correct for Zen,
//! where real durability happens through a separate, Zen-specific
//! function (`persist_workflow_step`) that bypasses `Journal` entirely -
//! but that means it doesn't honor this trait's own documented contract
//! ("durably append a completed step - result committed before execution
//! continues"). `SqliteJournal::append` does a real, synchronous SQL write
//! before returning, so a caller that actually relies on the trait's
//! contract (as Flux's durable functions do) gets what it asked for.

use std::collections::HashMap;

use rusqlite::{params, Connection, OptionalExtension};

use crate::journal::{InstanceId, Journal, RegisteredInstance, ResumeState, Signal, StepId, StepOutcome, StepRecord};
use crate::values::{json_to_value, value_to_json, Value};

pub struct SqliteJournal {
    conn: Connection,
    instance: InstanceId,
    cache: HashMap<StepId, StepRecord>,
}

impl SqliteJournal {
    /// Opens (creating if needed) `durable_steps`/`durable_instances` at
    /// `db_path`, scoped to `instance`. Does **not** prefetch existing
    /// rows - call `resume` (via the `Journal` trait) to populate the
    /// cache from storage, matching `WorkflowPersistence`'s existing
    /// "prefetch once" pattern (see its own `resume` doc comment).
    pub fn open(db_path: &str, instance: InstanceId) -> Result<Self, String> {
        let conn = Connection::open(db_path).map_err(|error| format!("Failed to open '{db_path}': {error}"))?;
        // rusqlite defaults to a 0ms busy timeout (an immediate `SQLITE_BUSY`
        // error on any write that collides with another connection's
        // in-flight transaction, rather than waiting). Harmless when this
        // type is only ever used by one connection at a time, but
        // `try_lock_instance`'s whole point is to be correct under genuine
        // concurrent access from separate connections/processes — without
        // this, a real collision could surface as a raw SQLite error instead
        // of `try_lock_instance`'s own clean `Ok(false)`/lock-conflict path.
        conn.busy_timeout(std::time::Duration::from_millis(5000))
            .map_err(|error| format!("Failed to set busy_timeout on '{db_path}': {error}"))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS durable_steps (
                instance_id TEXT NOT NULL,
                call_site TEXT NOT NULL,
                loop_key INTEGER NOT NULL,
                status TEXT NOT NULL,
                result_json TEXT,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (instance_id, call_site, loop_key)
            );
            CREATE TABLE IF NOT EXISTS durable_instances (
                instance_id TEXT PRIMARY KEY,
                fn_name TEXT NOT NULL,
                args_json TEXT NOT NULL,
                source TEXT NOT NULL,
                created_at TEXT NOT NULL,
                completed_at TEXT
            );
            CREATE TABLE IF NOT EXISTS durable_instance_locks (
                instance_id TEXT PRIMARY KEY,
                locked_at TEXT NOT NULL,
                pid INTEGER NOT NULL
            )",
        )
        .map_err(|error| format!("Failed to initialize durable_steps/durable_instances/durable_instance_locks tables: {error}"))?;
        Ok(Self { conn, instance, cache: HashMap::new() })
    }

    /// Opens a journal for dispatcher use - `register_instance`/
    /// `list_suspended_instances`/`lookup_instance` are all either
    /// instance-agnostic or take an explicit `instance` argument, so no
    /// single `InstanceId` needs to be known up front the way
    /// `append`/`suspend`/`deliver`/`resume` require (only those four ever
    /// read `self.instance`). Internally just `open` with a placeholder
    /// empty `InstanceId` those four never consult - only call the three
    /// dispatch-oriented methods against a journal opened this way.
    pub fn open_for_dispatch(db_path: &str) -> Result<Self, String> {
        Self::open(db_path, InstanceId(String::new()))
    }
}

/// `loop_key`'s absence (`None`) is encoded as `-1` - `u64` values are
/// always non-negative, so this is an unambiguous sentinel, and it sidesteps
/// relying on how a given SQL engine treats `NULL` inside a composite
/// primary key (repeated `NULL`s aren't guaranteed to collide for
/// uniqueness purposes).
fn loop_key_to_sql(loop_key: Option<u64>) -> i64 {
    loop_key.map(|k| k as i64).unwrap_or(-1)
}

fn loop_key_from_sql(stored: i64) -> Option<u64> {
    if stored < 0 {
        None
    } else {
        Some(stored as u64)
    }
}

fn status_and_payload(outcome: &StepOutcome) -> (&'static str, Option<String>) {
    match outcome {
        StepOutcome::Done(value) => ("done", Some(value_to_json(value).to_string())),
        StepOutcome::Failed(reason) => ("failed", Some(reason.clone())),
        StepOutcome::Suspended => ("suspended", None),
    }
}

fn outcome_from_row(status: &str, payload: Option<String>) -> Result<StepOutcome, String> {
    match status {
        "done" => {
            let raw = payload.ok_or_else(|| "durable_steps row: 'done' with no result_json".to_string())?;
            let json: serde_json::Value =
                serde_json::from_str(&raw).map_err(|error| format!("Failed to parse journaled result: {error}"))?;
            Ok(StepOutcome::Done(json_to_value(json)))
        }
        "failed" => Ok(StepOutcome::Failed(payload.unwrap_or_default())),
        "suspended" => Ok(StepOutcome::Suspended),
        other => Err(format!("durable_steps row: unknown status '{other}'")),
    }
}

impl Journal for SqliteJournal {
    fn lookup(&self, id: &StepId) -> Option<StepRecord> {
        self.cache.get(id).cloned()
    }

    fn append(&mut self, id: StepId, outcome: StepOutcome) -> Result<(), String> {
        let (status, payload) = status_and_payload(&outcome);
        self.conn
            .execute(
                "INSERT INTO durable_steps (instance_id, call_site, loop_key, status, result_json, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))
                 ON CONFLICT (instance_id, call_site, loop_key)
                 DO UPDATE SET status = excluded.status, result_json = excluded.result_json, updated_at = excluded.updated_at",
                params![self.instance.0, id.call_site, loop_key_to_sql(id.loop_key), status, payload],
            )
            .map_err(|error| format!("Failed to durably append step '{}': {error}", id.call_site))?;
        self.cache.insert(id, StepRecord { outcome });
        Ok(())
    }

    fn suspend(&mut self, id: StepId, wakeup: Signal) -> Result<(), String> {
        // Same durable-write shape as `append`, storing the wakeup signal
        // (a bare string, not JSON-encoded) as the payload. Read back only
        // by `deliver`'s own raw SQL query below - `outcome_from_row`'s
        // "suspended" arm discards this payload when reconstructing an
        // in-memory `StepRecord`, since `StepOutcome::Suspended` carries no
        // data of its own.
        self.conn
            .execute(
                "INSERT INTO durable_steps (instance_id, call_site, loop_key, status, result_json, updated_at)
                 VALUES (?1, ?2, ?3, 'suspended', ?4, datetime('now'))
                 ON CONFLICT (instance_id, call_site, loop_key)
                 DO UPDATE SET status = 'suspended', result_json = excluded.result_json, updated_at = excluded.updated_at",
                params![self.instance.0, id.call_site, loop_key_to_sql(id.loop_key), wakeup.0],
            )
            .map_err(|error| format!("Failed to durably suspend step '{}': {error}", id.call_site))?;
        self.cache.insert(id, StepRecord { outcome: StepOutcome::Suspended });
        Ok(())
    }

    /// Finds whichever suspended step in this instance is waiting on
    /// `signal` and durably marks it `done` with `value` - manual delivery
    /// only (no automatic dispatcher/event-bus wakeup; a caller re-invokes
    /// the durable fn afterward to actually resume it, see
    /// `docs/DURABLE-EXECUTION.md` in the Flux repo). Deliberately a
    /// `SELECT` for the exact primary key first, then an `UPDATE` keyed by
    /// that exact tuple - not a single predicate-based
    /// `UPDATE ... WHERE status='suspended' AND result_json=?`, since
    /// SQLite's bundled build has no portable `LIMIT` on `UPDATE` and a
    /// predicate-only `UPDATE` would silently mark *every* suspended row
    /// sharing that signal string as done in one call. If more than one
    /// suspended step happens to share the exact same signal string, this
    /// matches whichever row the `SELECT` returns first - no fan-out/
    /// multi-waiter design, a documented scope cut, not solved here.
    fn deliver(&mut self, signal: Signal, value: Value) -> Result<Option<StepId>, String> {
        let found: Option<(String, i64)> = self
            .conn
            .query_row(
                "SELECT call_site, loop_key FROM durable_steps
                 WHERE instance_id = ?1 AND status = 'suspended' AND result_json = ?2
                 LIMIT 1",
                params![self.instance.0, signal.0],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| format!("Failed to query for a step awaiting signal '{}': {error}", signal.0))?;

        let Some((call_site, loop_key)) = found else {
            return Ok(None);
        };

        let payload = value_to_json(&value).to_string();
        self.conn
            .execute(
                "UPDATE durable_steps SET status = 'done', result_json = ?4, updated_at = datetime('now')
                 WHERE instance_id = ?1 AND call_site = ?2 AND loop_key = ?3",
                params![self.instance.0, call_site, loop_key, payload],
            )
            .map_err(|error| format!("Failed to durably deliver signal '{}': {error}", signal.0))?;

        let id = StepId { call_site, loop_key: loop_key_from_sql(loop_key) };
        self.cache.insert(id.clone(), StepRecord { outcome: StepOutcome::Done(value) });
        Ok(Some(id))
    }

    fn resume(&mut self, instance: InstanceId) -> Result<ResumeState, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT call_site, loop_key, status, result_json FROM durable_steps WHERE instance_id = ?1")
            .map_err(|error| format!("Failed to prepare resume query: {error}"))?;
        let rows = stmt
            .query_map(params![instance.0], |row| {
                let call_site: String = row.get(0)?;
                let loop_key: i64 = row.get(1)?;
                let status: String = row.get(2)?;
                let payload: Option<String> = row.get(3)?;
                Ok((call_site, loop_key, status, payload))
            })
            .map_err(|error| format!("Failed to run resume query: {error}"))?;

        let mut records = HashMap::new();
        for row in rows {
            let (call_site, loop_key, status, payload) =
                row.map_err(|error| format!("Failed to read a durable_steps row: {error}"))?;
            let id = StepId { call_site, loop_key: loop_key_from_sql(loop_key) };
            let outcome = outcome_from_row(&status, payload)?;
            records.insert(id, StepRecord { outcome });
        }
        self.cache = records.clone();
        Ok(ResumeState { records })
    }

    fn register_instance(
        &mut self,
        instance: InstanceId,
        fn_name: String,
        args: Vec<Value>,
        source: String,
    ) -> Result<(), String> {
        let args_json = value_to_json(&Value::List(args)).to_string();
        self.conn
            .execute(
                "INSERT OR IGNORE INTO durable_instances (instance_id, fn_name, args_json, source, created_at)
                 VALUES (?1, ?2, ?3, ?4, datetime('now'))",
                params![instance.0, fn_name, args_json, source],
            )
            .map_err(|error| format!("Failed to register instance '{}': {error}", instance.0))?;
        Ok(())
    }

    fn list_incomplete_instances(&self) -> Result<Vec<InstanceId>, String> {
        // Deliberately queries `durable_instances.completed_at`, NOT
        // `durable_steps.status = 'suspended'` — `deliver` already flips a
        // delivered step's own row straight to 'done', independent of
        // whether the durable fn's remaining orchestration body has
        // actually been re-run past that point. A registered-but-not-yet-
        // fully-resumed instance must stay discoverable even though none
        // of its steps currently read 'suspended' any more.
        let mut stmt = self
            .conn
            .prepare("SELECT instance_id FROM durable_instances WHERE completed_at IS NULL")
            .map_err(|error| format!("Failed to prepare list_incomplete_instances query: {error}"))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| format!("Failed to run list_incomplete_instances query: {error}"))?;
        let mut instances = Vec::new();
        for row in rows {
            instances.push(InstanceId(
                row.map_err(|error| format!("Failed to read a durable_instances row: {error}"))?,
            ));
        }
        Ok(instances)
    }

    fn mark_instance_completed(&mut self, instance: InstanceId) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE durable_instances SET completed_at = datetime('now') WHERE instance_id = ?1",
                params![instance.0],
            )
            .map_err(|error| format!("Failed to mark instance '{}' completed: {error}", instance.0))?;
        Ok(())
    }

    fn lookup_instance(&self, instance: &InstanceId) -> Result<Option<RegisteredInstance>, String> {
        let row: Option<(String, String, String)> = self
            .conn
            .query_row(
                "SELECT fn_name, args_json, source FROM durable_instances WHERE instance_id = ?1",
                params![instance.0],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|error| format!("Failed to query registered instance '{}': {error}", instance.0))?;

        let Some((fn_name, args_json, source)) = row else {
            return Ok(None);
        };
        let json: serde_json::Value = serde_json::from_str(&args_json)
            .map_err(|error| format!("Failed to parse registered instance args: {error}"))?;
        let args = match json_to_value(json) {
            Value::List(items) => items,
            other => {
                return Err(format!(
                    "registered instance '{}' args_json did not decode to a list, got {other:?}",
                    instance.0
                ))
            }
        };
        Ok(Some(RegisteredInstance { fn_name, args, source }))
    }

    /// Claims the exclusive per-instance replay lock, transparently
    /// retrying a bounded number of times (short randomized backoff
    /// between attempts) on a transient `SQLITE_BUSY` before giving up.
    ///
    /// `open` already sets a 5s `busy_timeout` on this connection so
    /// SQLite's own busy handler retries internally too - but empirically
    /// that alone is not sufficient here: `zen-runtime/tests/
    /// try_lock_instance_stress.rs` reproduces many real, separate
    /// connections racing to write the same file and getting a raw
    /// `SQLITE_BUSY` ("database is locked") back in well under a
    /// millisecond, nowhere near the configured timeout window
    /// (busy_timeout governs how long a single
    /// blocked *lock acquisition* sleeps/retries inside one `step()` call;
    /// it doesn't guarantee every contending connection actually gets a
    /// chance to acquire the lock before giving up). Retrying the whole
    /// claim transaction here, at the application level, is what actually
    /// closes that gap - this is this codebase's real, previously flaky
    /// two-watcher-on-one-journal regression (`flux-cli/tests/run.rs`'s
    /// `two_watchers_on_the_same_journal_file_both_still_resume_correctly`).
    fn try_lock_instance(&mut self, instance: &InstanceId) -> Result<bool, String> {
        let pid = std::process::id();
        const MAX_ATTEMPTS: u32 = 100;
        let mut attempt: u32 = 0;
        loop {
            match self.try_lock_instance_once(instance, pid) {
                Ok(acquired) => return Ok(acquired),
                Err(error) if error.is_busy() && attempt + 1 < MAX_ATTEMPTS => {
                    attempt += 1;
                    std::thread::sleep(retry_backoff(attempt));
                }
                Err(error) => return Err(error.into_message(&instance.0)),
            }
        }
    }

    fn unlock_instance(&mut self, instance: &InstanceId) -> Result<(), String> {
        self.conn
            .execute(
                "DELETE FROM durable_instance_locks WHERE instance_id = ?1 AND pid = ?2",
                params![instance.0, std::process::id()],
            )
            .map_err(|error| format!("Failed to release lock for '{}': {error}", instance.0))?;
        Ok(())
    }
}

impl SqliteJournal {
    /// One attempt at the lock-claim transaction `try_lock_instance` wraps
    /// with retries - identical logic to what used to be inline in
    /// `try_lock_instance` itself, just returning the raw
    /// [`LockAttemptError`] (which step failed, and the underlying
    /// `rusqlite::Error`) instead of an already-formatted `String`, so the
    /// caller can distinguish a retryable `SQLITE_BUSY` from everything
    /// else before deciding whether to give up.
    fn try_lock_instance_once(&mut self, instance: &InstanceId, pid: u32) -> Result<bool, LockAttemptError> {
        let tx = self.conn.transaction().map_err(LockAttemptError::Transaction)?;
        let existing: Option<i64> = tx
            .query_row(
                "SELECT pid FROM durable_instance_locks WHERE instance_id = ?1",
                params![instance.0],
                |row| row.get(0),
            )
            .optional()
            .map_err(LockAttemptError::ReadLockRow)?;
        if let Some(holder_pid) = existing {
            if pid_is_alive(holder_pid as u32) {
                return Ok(false);
            }
            tx.execute(
                "UPDATE durable_instance_locks SET locked_at = datetime('now'), pid = ?2 WHERE instance_id = ?1",
                params![instance.0, pid],
            )
        } else {
            tx.execute(
                "INSERT INTO durable_instance_locks (instance_id, locked_at, pid) VALUES (?1, datetime('now'), ?2)",
                params![instance.0, pid],
            )
        }
        .map_err(LockAttemptError::ClaimWrite)?;
        tx.commit().map_err(LockAttemptError::Commit)?;
        Ok(true)
    }
}

/// Which step of the lock-claim transaction failed, carrying the
/// underlying `rusqlite::Error` so `try_lock_instance` can both (a) check
/// whether it's a retryable `SQLITE_BUSY` and (b) - once retries are
/// exhausted or the error isn't busy-related - reproduce the exact same
/// per-step message `try_lock_instance` used to format inline.
enum LockAttemptError {
    Transaction(rusqlite::Error),
    ReadLockRow(rusqlite::Error),
    ClaimWrite(rusqlite::Error),
    Commit(rusqlite::Error),
}

impl LockAttemptError {
    fn is_busy(&self) -> bool {
        let (Self::Transaction(error) | Self::ReadLockRow(error) | Self::ClaimWrite(error) | Self::Commit(error)) = self;
        matches!(
            error,
            rusqlite::Error::SqliteFailure(inner, _) if inner.code == rusqlite::ErrorCode::DatabaseBusy
        )
    }

    fn into_message(self, instance_id: &str) -> String {
        match self {
            Self::Transaction(error) => format!("Failed to start lock transaction: {error}"),
            Self::ReadLockRow(error) => format!("Failed to read lock row for '{instance_id}': {error}"),
            Self::ClaimWrite(error) => format!("Failed to claim lock for '{instance_id}': {error}"),
            Self::Commit(error) => format!("Failed to commit lock claim for '{instance_id}': {error}"),
        }
    }
}

/// Dependency-free jitter for `try_lock_instance`'s retry backoff - same
/// construction as `a_journaled_float_round_trips_exactly_across_many_real_values`'s
/// stress test below (hash fresh OS-seeded entropy plus the current time
/// into a small range). Not cryptographically random, just enough spread
/// that many contending threads/processes don't retry in lockstep and
/// re-collide every time. Ramps the base delay up (capped) with the
/// attempt count so sustained contention backs off rather than hammering
/// the file at a fixed rate.
fn retry_backoff(attempt: u32) -> std::time::Duration {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};

    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u32(attempt);
    hasher.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
    let jitter_ms = hasher.finish() % 8; // 0..=7ms of jitter
    let base_ms = u64::from(attempt).saturating_mul(2).min(50); // linear ramp, capped at 50ms
    std::time::Duration::from_millis(base_ms + jitter_ms)
}

/// Whether `pid` currently identifies a running OS process — used by
/// `try_lock_instance` to decide whether an existing lock row was
/// abandoned by a crashed holder (safe to reclaim) or still belongs to a
/// live one (must not steal it). A known, accepted, industry-standard
/// limitation: PID reuse could in rare cases make a dead-then-recycled PID
/// look alive again, or (far more narrowly) a lock check could race a
/// brand-new process being assigned the exact PID just freed by the real
/// holder's exit — single-machine only, no cross-host story.
fn pid_is_alive(pid: u32) -> bool {
    let mut system = sysinfo::System::new_all();
    system.refresh_all();
    system
        .processes()
        .keys()
        .any(|candidate| candidate.as_u32() == pid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn temp_db_path(name: &str) -> String {
        env::temp_dir()
            .join(format!("zen-runtime-durable-journal-test-{}-{}.db", std::process::id(), name))
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn append_then_lookup_round_trips_a_done_outcome() {
        let path = temp_db_path("roundtrip");
        let mut journal = SqliteJournal::open(&path, InstanceId("instance-1".into())).unwrap();
        let id = StepId { call_site: "compile#0".into(), loop_key: None };

        journal.append(id.clone(), StepOutcome::Done(Value::String("9c...".into()))).unwrap();

        let record = journal.lookup(&id).expect("expected a cached record");
        assert!(matches!(record.outcome, StepOutcome::Done(Value::String(s)) if s == "9c..."));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn append_is_really_durable_across_a_fresh_connection() {
        // The whole point of this type: open, append, drop the connection
        // entirely, reopen fresh, resume - the row must already be there,
        // proving `append` wrote to disk synchronously rather than only
        // updating an in-memory cache (which `WorkflowPersistence::append`
        // deliberately does, and which this type exists to not do).
        let path = temp_db_path("durable-across-reopen");
        let instance = InstanceId("instance-1".into());
        let id = StepId { call_site: "compile#0".into(), loop_key: None };

        {
            let mut journal = SqliteJournal::open(&path, InstanceId(instance.0.clone())).unwrap();
            journal.append(id.clone(), StepOutcome::Done(Value::Number(42.0))).unwrap();
            // journal (and its Connection) dropped here - no explicit flush/close.
        }

        let mut reopened = SqliteJournal::open(&path, InstanceId(instance.0.clone())).unwrap();
        let resumed = reopened.resume(InstanceId(instance.0.clone())).unwrap();
        let record = resumed.records.get(&id).expect("expected the row to have survived reopening the connection");
        assert!(matches!(record.outcome, StepOutcome::Done(Value::Number(n)) if n == 42.0));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn resume_populates_the_cache_so_lookup_works_without_a_prior_append() {
        let path = temp_db_path("resume-then-lookup");
        let instance = InstanceId("instance-1".into());
        let id = StepId { call_site: "run-tests#1".into(), loop_key: None };

        {
            let mut journal = SqliteJournal::open(&path, InstanceId(instance.0.clone())).unwrap();
            journal.append(id.clone(), StepOutcome::Done(Value::Bool(true))).unwrap();
        }

        let mut reopened = SqliteJournal::open(&path, InstanceId(instance.0.clone())).unwrap();
        reopened.resume(InstanceId(instance.0.clone())).unwrap();
        let record = reopened.lookup(&id).expect("expected resume to have populated the cache");
        assert!(matches!(record.outcome, StepOutcome::Done(Value::Bool(true))));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn different_instances_do_not_see_each_others_steps() {
        let path = temp_db_path("instance-isolation");
        let id = StepId { call_site: "compile#0".into(), loop_key: None };

        let mut journal_a = SqliteJournal::open(&path, InstanceId("instance-a".into())).unwrap();
        journal_a.append(id.clone(), StepOutcome::Done(Value::Number(1.0))).unwrap();

        let mut journal_b = SqliteJournal::open(&path, InstanceId("instance-b".into())).unwrap();
        let resumed_b = journal_b.resume(InstanceId("instance-b".into())).unwrap();
        assert!(!resumed_b.records.contains_key(&id), "instance-b should not see instance-a's step");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn suspend_then_resume_round_trips_a_suspended_outcome() {
        let path = temp_db_path("suspend-roundtrip");
        let instance = InstanceId("instance-1".into());
        let id = StepId { call_site: "approval#2".into(), loop_key: None };

        {
            let mut journal = SqliteJournal::open(&path, InstanceId(instance.0.clone())).unwrap();
            journal.suspend(id.clone(), Signal("ship-approved".into())).unwrap();
        }

        let mut reopened = SqliteJournal::open(&path, InstanceId(instance.0.clone())).unwrap();
        let resumed = reopened.resume(InstanceId(instance.0.clone())).unwrap();
        assert!(matches!(resumed.records.get(&id).unwrap().outcome, StepOutcome::Suspended));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn deliver_finds_a_suspended_step_and_marks_it_done() {
        let path = temp_db_path("deliver-basic");
        let mut journal = SqliteJournal::open(&path, InstanceId("instance-1".into())).unwrap();
        let id = StepId { call_site: "approval#2".into(), loop_key: None };

        journal.suspend(id.clone(), Signal("ship-approved".into())).unwrap();
        let delivered = journal.deliver(Signal("ship-approved".into()), Value::Bool(true)).unwrap();

        assert_eq!(delivered, Some(id.clone()));
        let record = journal.lookup(&id).unwrap();
        assert!(matches!(record.outcome, StepOutcome::Done(Value::Bool(true))));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn deliver_with_no_matching_signal_returns_none() {
        let path = temp_db_path("deliver-no-match");
        let mut journal = SqliteJournal::open(&path, InstanceId("instance-1".into())).unwrap();

        let delivered = journal.deliver(Signal("nothing-waiting".into()), Value::Bool(true)).unwrap();
        assert!(delivered.is_none());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn deliver_then_resume_round_trips_the_delivered_value() {
        // Same "drop the connection, reopen fresh" proof as
        // `append_is_really_durable_across_a_fresh_connection` - the
        // delivered value must be readable by a completely separate
        // process/connection, not just the one that delivered it.
        let path = temp_db_path("deliver-durable-across-reopen");
        let instance = InstanceId("instance-1".into());
        let id = StepId { call_site: "approval#2".into(), loop_key: None };

        {
            let mut journal = SqliteJournal::open(&path, InstanceId(instance.0.clone())).unwrap();
            journal.suspend(id.clone(), Signal("ship-approved".into())).unwrap();
            journal.deliver(Signal("ship-approved".into()), Value::Number(42.0)).unwrap();
            // journal (and its Connection) dropped here.
        }

        let mut reopened = SqliteJournal::open(&path, InstanceId(instance.0.clone())).unwrap();
        let resumed = reopened.resume(InstanceId(instance.0.clone())).unwrap();
        let record = resumed.records.get(&id).expect("expected the delivered row to have survived reopening");
        assert!(matches!(record.outcome, StepOutcome::Done(Value::Number(n)) if n == 42.0));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn deliver_only_affects_the_matching_instance() {
        let path = temp_db_path("deliver-instance-isolation");
        let id = StepId { call_site: "approval#2".into(), loop_key: None };

        let mut journal_a = SqliteJournal::open(&path, InstanceId("instance-a".into())).unwrap();
        journal_a.suspend(id.clone(), Signal("ship-approved".into())).unwrap();

        let mut journal_b = SqliteJournal::open(&path, InstanceId("instance-b".into())).unwrap();
        journal_b.suspend(id.clone(), Signal("ship-approved".into())).unwrap();

        let delivered = journal_a.deliver(Signal("ship-approved".into()), Value::Bool(true)).unwrap();
        assert_eq!(delivered, Some(id.clone()));

        // instance-b's own suspended row must be untouched.
        let resumed_b = journal_b.resume(InstanceId("instance-b".into())).unwrap();
        assert!(matches!(resumed_b.records.get(&id).unwrap().outcome, StepOutcome::Suspended));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn deliver_updates_the_exact_row_not_every_row_sharing_the_signal() {
        // Two different steps in the same instance suspended on the exact
        // same signal string - delivering once must only mark one of them
        // done, proving the implementation keys its UPDATE by the exact
        // primary key found via SELECT, not a predicate-only
        // `WHERE status='suspended' AND result_json=?` that could match
        // (and update) every row sharing that string.
        let path = temp_db_path("deliver-exact-row");
        let mut journal = SqliteJournal::open(&path, InstanceId("instance-1".into())).unwrap();
        let id_1 = StepId { call_site: "approval#2".into(), loop_key: None };
        let id_2 = StepId { call_site: "approval#5".into(), loop_key: None };

        journal.suspend(id_1.clone(), Signal("ship-approved".into())).unwrap();
        journal.suspend(id_2.clone(), Signal("ship-approved".into())).unwrap();

        journal.deliver(Signal("ship-approved".into()), Value::Bool(true)).unwrap();

        let done_count = [&id_1, &id_2]
            .iter()
            .filter(|id| matches!(journal.lookup(id).unwrap().outcome, StepOutcome::Done(_)))
            .count();
        assert_eq!(done_count, 1, "expected exactly one of the two suspended rows to become Done");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn append_twice_for_the_same_step_upserts_rather_than_erroring() {
        let path = temp_db_path("upsert");
        let mut journal = SqliteJournal::open(&path, InstanceId("instance-1".into())).unwrap();
        let id = StepId { call_site: "compile#0".into(), loop_key: None };

        journal.append(id.clone(), StepOutcome::Failed("transient".into())).unwrap();
        journal.append(id.clone(), StepOutcome::Done(Value::Bool(true))).unwrap();

        let record = journal.lookup(&id).unwrap();
        assert!(matches!(record.outcome, StepOutcome::Done(Value::Bool(true))));

        let _ = std::fs::remove_file(&path);
    }

    /// Regression test for a real bug: a journaled `Value::Number(f64)`
    /// round-trips through `value_to_json`/`json_to_value` as a JSON string
    /// stored in `result_json` (a TEXT column) - `append` writes it,
    /// `resume` re-parses it via `serde_json::from_str::<serde_json::Value>`.
    /// That parse goes through serde_json's generic `deserialize_any` path,
    /// which - without the `float_roundtrip` Cargo feature - uses a fast
    /// but not-always-correctly-rounded f64 parser: off by 1 ULP for roughly
    /// 10% of arbitrary floats in `[0, 1)` (measured empirically). A single
    /// run only samples one random float, so this was easy to miss locally
    /// and only showed up "occasionally" once enough independent test runs
    /// (e.g. a parallel `cargo test --workspace`) drove the sample count up.
    /// Sampling many floats here, in one run, makes that failure rate
    /// visible directly instead of relying on running the same test many
    /// times and hoping to get unlucky.
    #[test]
    fn a_journaled_float_round_trips_exactly_across_many_real_values() {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};

        let path = temp_db_path("float-roundtrip-stress");
        let instance = InstanceId("instance-1".into());
        let mut journal = SqliteJournal::open(&path, InstanceId(instance.0.clone())).unwrap();

        let mut mismatches = Vec::new();
        for i in 0..2000u64 {
            // Same construction Flux's `rand_builtin` uses: hash fresh
            // per-process OS-seeded entropy into a float in [0, 1).
            let mut hasher = RandomState::new().build_hasher();
            hasher.write_u64(i);
            hasher.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
            let bits = hasher.finish();
            let original = (bits as f64) / (u64::MAX as f64 + 1.0);

            let id = StepId { call_site: format!("stress#{i}"), loop_key: None };
            journal.append(id.clone(), StepOutcome::Done(Value::Number(original))).unwrap();

            // Force the real round trip: re-read from SQLite and re-parse
            // the JSON text, exactly as a second `run_durable` call's
            // `resume` does on replay - not just a cache hit.
            let resumed = journal.resume(InstanceId(instance.0.clone())).unwrap();
            let record = resumed.records.get(&id).unwrap();
            match record.outcome {
                StepOutcome::Done(Value::Number(replayed)) if replayed.to_bits() == original.to_bits() => {}
                StepOutcome::Done(Value::Number(replayed)) => mismatches.push((original, replayed)),
                ref other => panic!("expected Done(Number(_)), got {other:?}"),
            }
        }

        assert!(
            mismatches.is_empty(),
            "{} of 2000 journaled floats did not round-trip exactly, e.g. {:?}",
            mismatches.len(),
            &mismatches[..mismatches.len().min(5)]
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn register_instance_then_lookup_round_trips() {
        let path = temp_db_path("register-roundtrip");
        let mut journal = SqliteJournal::open(&path, InstanceId("instance-1".into())).unwrap();

        journal
            .register_instance(
                InstanceId("instance-1".into()),
                "release".into(),
                vec![Value::Number(42.0)],
                "durable fn release(n) { await(\"go\") }".into(),
            )
            .unwrap();

        let registered = journal.lookup_instance(&InstanceId("instance-1".into())).unwrap().unwrap();
        assert_eq!(registered.fn_name, "release");
        assert!(matches!(registered.args.as_slice(), [Value::Number(n)] if *n == 42.0));
        assert_eq!(registered.source, "durable fn release(n) { await(\"go\") }");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn register_instance_is_idempotent_first_write_wins() {
        let path = temp_db_path("register-idempotent");
        let mut journal = SqliteJournal::open(&path, InstanceId("instance-1".into())).unwrap();

        journal
            .register_instance(InstanceId("instance-1".into()), "first_fn".into(), vec![Value::Number(1.0)], "src-1".into())
            .unwrap();
        journal
            .register_instance(InstanceId("instance-1".into()), "second_fn".into(), vec![Value::Number(2.0)], "src-2".into())
            .unwrap();

        let registered = journal.lookup_instance(&InstanceId("instance-1".into())).unwrap().unwrap();
        assert_eq!(registered.fn_name, "first_fn", "expected the first registration to win");
        assert_eq!(registered.source, "src-1");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn lookup_instance_with_no_registration_returns_none() {
        let path = temp_db_path("lookup-no-registration");
        let journal = SqliteJournal::open(&path, InstanceId("instance-1".into())).unwrap();

        let registered = journal.lookup_instance(&InstanceId("never-registered".into())).unwrap();
        assert!(registered.is_none());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn register_instance_is_durable_across_a_fresh_connection() {
        let path = temp_db_path("register-durable-across-reopen");
        let instance = InstanceId("instance-1".into());

        {
            let mut journal = SqliteJournal::open(&path, InstanceId(instance.0.clone())).unwrap();
            journal
                .register_instance(InstanceId(instance.0.clone()), "release".into(), vec![], "src".into())
                .unwrap();
            // journal (and its Connection) dropped here.
        }

        let reopened = SqliteJournal::open(&path, InstanceId(instance.0.clone())).unwrap();
        let registered = reopened.lookup_instance(&InstanceId(instance.0.clone())).unwrap();
        assert!(registered.is_some(), "expected the registration to have survived reopening the connection");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn list_incomplete_instances_finds_multiple_registered_instances_and_excludes_a_completed_one() {
        let path = temp_db_path("list-incomplete-multiple");
        let mut journal_a = SqliteJournal::open(&path, InstanceId("instance-a".into())).unwrap();
        journal_a
            .register_instance(InstanceId("instance-a".into()), "f".into(), vec![], "src".into())
            .unwrap();
        journal_a
            .suspend(StepId { call_site: "approval#0".into(), loop_key: None }, Signal("go-a".into()))
            .unwrap();
        let mut journal_b = SqliteJournal::open(&path, InstanceId("instance-b".into())).unwrap();
        journal_b
            .register_instance(InstanceId("instance-b".into()), "f".into(), vec![], "src".into())
            .unwrap();
        journal_b
            .suspend(StepId { call_site: "approval#0".into(), loop_key: None }, Signal("go-b".into()))
            .unwrap();
        let mut journal_c = SqliteJournal::open(&path, InstanceId("instance-c".into())).unwrap();
        journal_c
            .register_instance(InstanceId("instance-c".into()), "f".into(), vec![], "src".into())
            .unwrap();
        journal_c
            .append(StepId { call_site: "compile#0".into(), loop_key: None }, StepOutcome::Done(Value::Bool(true)))
            .unwrap();
        journal_c.mark_instance_completed(InstanceId("instance-c".into())).unwrap();

        let mut incomplete: Vec<String> = journal_c.list_incomplete_instances().unwrap().into_iter().map(|id| id.0).collect();
        incomplete.sort();
        assert_eq!(incomplete, vec!["instance-a".to_string(), "instance-b".to_string()]);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn list_incomplete_instances_still_includes_an_instance_after_delivery_until_marked_completed() {
        // The bug this guards against: `deliver` flips the delivered
        // step's own `durable_steps` row straight to 'done', independent
        // of whether the durable fn's remaining orchestration body has
        // ever actually been re-run. A dispatcher relying on "no longer
        // has a suspended step" to mean "done" would silently stop
        // discovering an instance the moment it's delivered, even though
        // nobody has resumed it to real completion yet.
        let path = temp_db_path("list-incomplete-then-delivered");
        let mut journal = SqliteJournal::open(&path, InstanceId("instance-1".into())).unwrap();
        journal
            .register_instance(InstanceId("instance-1".into()), "f".into(), vec![], "src".into())
            .unwrap();
        journal
            .suspend(StepId { call_site: "approval#0".into(), loop_key: None }, Signal("go".into()))
            .unwrap();

        let incomplete = journal.list_incomplete_instances().unwrap();
        assert!(incomplete.iter().any(|id| id.0 == "instance-1"), "expected instance-1 to be listed while suspended");

        journal.deliver(Signal("go".into()), Value::Bool(true)).unwrap();

        let incomplete_after_delivery = journal.list_incomplete_instances().unwrap();
        assert!(
            incomplete_after_delivery.iter().any(|id| id.0 == "instance-1"),
            "expected instance-1 to STILL be listed after delivery - delivery alone doesn't mean resumed"
        );

        journal.mark_instance_completed(InstanceId("instance-1".into())).unwrap();

        let incomplete_after_completion = journal.list_incomplete_instances().unwrap();
        assert!(
            !incomplete_after_completion.iter().any(|id| id.0 == "instance-1"),
            "expected instance-1 to no longer be listed once explicitly marked completed"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn open_for_dispatch_sees_rows_written_via_a_plain_open_connection() {
        let path = temp_db_path("open-for-dispatch");
        {
            let mut journal = SqliteJournal::open(&path, InstanceId("instance-1".into())).unwrap();
            journal
                .register_instance(InstanceId("instance-1".into()), "release".into(), vec![], "src".into())
                .unwrap();
            journal
                .suspend(StepId { call_site: "approval#0".into(), loop_key: None }, Signal("go".into()))
                .unwrap();
        }

        let dispatch_journal = SqliteJournal::open_for_dispatch(&path).unwrap();
        let incomplete = dispatch_journal.list_incomplete_instances().unwrap();
        assert!(incomplete.iter().any(|id| id.0 == "instance-1"));
        let registered = dispatch_journal.lookup_instance(&InstanceId("instance-1".into())).unwrap();
        assert!(registered.is_some(), "expected open_for_dispatch's placeholder InstanceId to not scope lookup_instance");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn try_lock_instance_succeeds_when_unlocked() {
        let path = temp_db_path("lock-fresh");
        let mut journal = SqliteJournal::open(&path, InstanceId("instance-1".into())).unwrap();

        let acquired = journal.try_lock_instance(&InstanceId("instance-1".into())).unwrap();
        assert!(acquired, "expected the lock to be acquired when no row exists yet");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn try_lock_instance_fails_when_already_locked_by_a_live_process() {
        let path = temp_db_path("lock-contended");
        let mut journal = SqliteJournal::open(&path, InstanceId("instance-1".into())).unwrap();
        let instance = InstanceId("instance-1".into());

        // Lock the row directly with this *test process's own* pid - it's
        // definitely alive, standing in for a genuinely concurrent holder.
        journal
            .conn
            .execute(
                "INSERT INTO durable_instance_locks (instance_id, locked_at, pid) VALUES (?1, datetime('now'), ?2)",
                params![instance.0, std::process::id()],
            )
            .unwrap();

        let acquired = journal.try_lock_instance(&instance).unwrap();
        assert!(!acquired, "expected the lock to be refused while a live process holds it");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn try_lock_instance_reclaims_a_lock_held_by_a_dead_pid() {
        let path = temp_db_path("lock-stale");
        let mut journal = SqliteJournal::open(&path, InstanceId("instance-1".into())).unwrap();
        let instance = InstanceId("instance-1".into());

        // A trivial, fast-exiting child process - waited on, so its pid is
        // genuinely dead by the time we use it, not just an assumed-unused
        // number that happens not to be running right now.
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "exit", "0"])
            .spawn()
            .expect("failed to spawn a throwaway child process");
        let dead_pid = child.id();
        child.wait().expect("failed to wait for the throwaway child process");

        journal
            .conn
            .execute(
                "INSERT INTO durable_instance_locks (instance_id, locked_at, pid) VALUES (?1, datetime('now'), ?2)",
                params![instance.0, dead_pid],
            )
            .unwrap();

        let acquired = journal.try_lock_instance(&instance).unwrap();
        assert!(acquired, "expected a lock held by a dead pid to be reclaimed");

        let holder_pid: i64 = journal
            .conn
            .query_row(
                "SELECT pid FROM durable_instance_locks WHERE instance_id = ?1",
                params![instance.0],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(holder_pid as u32, std::process::id(), "expected the row to now show this process's own pid");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unlock_instance_removes_the_row_and_a_second_lock_attempt_then_succeeds() {
        let path = temp_db_path("unlock");
        let mut journal = SqliteJournal::open(&path, InstanceId("instance-1".into())).unwrap();
        let instance = InstanceId("instance-1".into());

        assert!(journal.try_lock_instance(&instance).unwrap());
        journal.unlock_instance(&instance).unwrap();

        let row_count: i64 = journal
            .conn
            .query_row("SELECT COUNT(*) FROM durable_instance_locks WHERE instance_id = ?1", params![instance.0], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(row_count, 0, "expected unlock_instance to remove the row entirely");

        assert!(
            journal.try_lock_instance(&instance).unwrap(),
            "expected a second lock attempt to succeed once the first was released"
        );

        let _ = std::fs::remove_file(&path);
    }
}
