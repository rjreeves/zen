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

use crate::journal::{InstanceId, Journal, ResumeState, Signal, StepId, StepOutcome, StepRecord};
use crate::values::{json_to_value, value_to_json, Value};

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
}
