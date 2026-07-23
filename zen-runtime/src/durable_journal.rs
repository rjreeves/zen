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

use rusqlite::{params, Connection};

use crate::journal::{InstanceId, Journal, ResumeState, Signal, StepId, StepOutcome, StepRecord};
use crate::values::{json_to_value, value_to_json};

pub struct SqliteJournal {
    conn: Connection,
    instance: InstanceId,
    cache: HashMap<StepId, StepRecord>,
}

impl SqliteJournal {
    /// Opens (creating if needed) a durable-steps table at `db_path`,
    /// scoped to `instance`. Does **not** prefetch existing rows - call
    /// `resume` (via the `Journal` trait) to populate the cache from
    /// storage, matching `WorkflowPersistence`'s existing "prefetch once"
    /// pattern (see its own `resume` doc comment).
    pub fn open(db_path: &str, instance: InstanceId) -> Result<Self, String> {
        let conn = Connection::open(db_path).map_err(|error| format!("Failed to open '{db_path}': {error}"))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS durable_steps (
                instance_id TEXT NOT NULL,
                call_site TEXT NOT NULL,
                loop_key INTEGER NOT NULL,
                status TEXT NOT NULL,
                result_json TEXT,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (instance_id, call_site, loop_key)
            )",
        )
        .map_err(|error| format!("Failed to initialize durable_steps table: {error}"))?;
        Ok(Self { conn, instance, cache: HashMap::new() })
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
        // Same durable-write shape as `append`, storing the wakeup signal as
        // the payload. Unreachable from Flux this slice (`await` is cut -
        // see docs/PHASE3-PLAN.md's 3.4 scope cuts) but implemented for
        // real, not stubbed, since it's part of the same trait.
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::values::Value;
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
}
