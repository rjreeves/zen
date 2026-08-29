//! Regression coverage for a real, previously-flaky bug in `SqliteJournal`'s
//! ordinary read/write methods (`src/durable_journal.rs`): `append`,
//! `suspend`, `deliver`, `resume`, `register_instance`,
//! `list_incomplete_instances`, `mark_instance_completed`,
//! `lookup_instance`, and `unlock_instance` each ran a plain, single-shot
//! SQL statement with no retry of their own, relying solely on the
//! connection's `busy_timeout(5s)` (set in `open`) to ride out contention
//! from another connection on the same file.
//!
//! `try_lock_instance_stress.rs` already established that `busy_timeout`
//! alone isn't sufficient for `try_lock_instance`'s claim transaction under
//! real concurrent access - measured empirically, a raw `SQLITE_BUSY` can
//! come back in well under a millisecond, nowhere near the 5s window. This
//! test shows the exact same class of bug hits the "ordinary" methods too,
//! not just the schema-init/lock-claim call sites that originally motivated
//! the fix: with enough real concurrent pressure, `register_instance`
//! reliably surfaces a raw `"database is locked"` error instead of quietly
//! retrying.
//!
//! This is what made `flux-cli/tests/run.rs`'s
//! `watch_mode_automatically_resumes_once_delivered_by_another_process`
//! flaky under a full `cargo test --workspace` run: a `--watch` poll loop
//! (register_instance/resume/suspend, guarded by try_lock_instance/
//! unlock_instance) and a concurrent `--deliver` call (open + deliver, no
//! locking at all) each open their own connection to the same journal file;
//! under enough real system load the unretried side can lose the race and
//! surface a raw busy error instead of the clean success/suspend/timeout
//! outcomes the test expects — two lone subprocesses only occasionally hit
//! the narrow window, which is why it reproduced maybe once in many runs.
//! This test manufactures the same race deterministically and with far
//! higher pressure - many threads, each with a fresh connection, hammering
//! every method back-to-back against one shared file - so it fails on
//! essentially every run without the fix in `retry_on_busy`, instead of
//! relying on incidental process-scheduling timing.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use zen_runtime::durable_journal::SqliteJournal;
use zen_runtime::journal::{InstanceId, Journal, Signal, StepId};
use zen_runtime::values::Value;

#[test]
fn many_threads_hammering_ordinary_journal_methods_never_surfaces_a_raw_busy_error() {
    let path = std::env::temp_dir()
        .join(format!("zen-runtime-journal-operations-stress-{}.db", std::process::id()))
        .to_string_lossy()
        .into_owned();
    let _ = std::fs::remove_file(&path);

    const POLLER_THREADS: usize = 24;
    const ATTEMPTS_PER_THREAD: usize = 60;

    let errors = Arc::new(AtomicUsize::new(0));

    // Each "poller" thread repeatedly does what a --watch poll loop does
    // for its own instance: open, lookup_instance, register_instance,
    // try_lock_instance, resume, suspend, unlock_instance.
    let handles: Vec<_> = (0..POLLER_THREADS)
        .map(|thread_index| {
            let path = path.clone();
            let errors = Arc::clone(&errors);
            std::thread::spawn(move || {
                for attempt in 0..ATTEMPTS_PER_THREAD {
                    let instance = InstanceId(format!("instance-{thread_index}"));
                    match SqliteJournal::open(&path, InstanceId(instance.0.clone())) {
                        Ok(mut journal) => {
                            if let Err(e) = journal.lookup_instance(&instance) {
                                eprintln!("lookup_instance failed: {e}");
                                errors.fetch_add(1, Ordering::SeqCst);
                            }
                            if let Err(e) = journal.register_instance(
                                InstanceId(instance.0.clone()),
                                "release".to_string(),
                                vec![Value::Null],
                                "durable fn release() { await(\"sig\") }".to_string(),
                            ) {
                                eprintln!("register_instance failed: {e}");
                                errors.fetch_add(1, Ordering::SeqCst);
                            }
                            match journal.try_lock_instance(&instance) {
                                Ok(true) => {
                                    if let Err(e) = journal.resume(InstanceId(instance.0.clone())) {
                                        eprintln!("resume failed: {e}");
                                        errors.fetch_add(1, Ordering::SeqCst);
                                    }
                                    let id = StepId { call_site: format!("await#{attempt}"), loop_key: None };
                                    if let Err(e) = journal.suspend(id, Signal("sig".to_string())) {
                                        eprintln!("suspend failed: {e}");
                                        errors.fetch_add(1, Ordering::SeqCst);
                                    }
                                    if let Err(e) = journal.unlock_instance(&instance) {
                                        eprintln!("unlock_instance failed: {e}");
                                        errors.fetch_add(1, Ordering::SeqCst);
                                    }
                                }
                                Ok(false) => {} // legitimately contended by another thread's lock - fine
                                Err(e) => {
                                    eprintln!("try_lock_instance failed: {e}");
                                    errors.fetch_add(1, Ordering::SeqCst);
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("open failed: {e}");
                            errors.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                }
            })
        })
        .collect();

    // One "deliverer" thread, matching flux-cli's --deliver: open + deliver,
    // no locking at all - the unretried side of the real race.
    let deliverer_errors = Arc::clone(&errors);
    let deliverer_path = path.clone();
    let deliverer = std::thread::spawn(move || {
        for thread_index in 0..POLLER_THREADS {
            for _ in 0..ATTEMPTS_PER_THREAD {
                let instance = InstanceId(format!("instance-{thread_index}"));
                match SqliteJournal::open(&deliverer_path, instance) {
                    Ok(mut journal) => {
                        if let Err(e) = journal.deliver(Signal("sig".to_string()), Value::Bool(true)) {
                            eprintln!("deliver failed: {e}");
                            deliverer_errors.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                    Err(e) => {
                        eprintln!("open (deliverer) failed: {e}");
                        deliverer_errors.fetch_add(1, Ordering::SeqCst);
                    }
                }
            }
        }
    });

    for handle in handles {
        handle.join().expect("poller thread panicked");
    }
    deliverer.join().expect("deliverer thread panicked");

    let _ = std::fs::remove_file(&path);
    assert_eq!(
        errors.load(Ordering::SeqCst),
        0,
        "expected every SqliteJournal method to transparently retry through SQLITE_BUSY under real \
         concurrent access, not surface it"
    );
}
