//! Regression coverage for a real, previously-flaky bug in
//! `SqliteJournal::try_lock_instance` (`src/durable_journal.rs`): many
//! separate connections racing to write the SAME sqlite file - even for
//! DIFFERENT instance ids, so there's no legitimate "instance already
//! locked" contention, only raw SQLite-level contention on the shared
//! file - could surface a raw `SQLITE_BUSY` ("database is locked") error
//! instead of being retried.
//!
//! This is what made
//! `flux-cli/tests/run.rs`'s
//! `two_watchers_on_the_same_journal_file_both_still_resume_correctly`
//! flaky: it only has two subprocesses each polling every 300ms, so the
//! actual race window is narrow and inconsistently hit - reproduced maybe
//! once in several runs. Setting `busy_timeout` on the connection (see
//! `open`'s doc comment) was a reasonable first fix but turned out not to
//! be sufficient by itself: measured empirically, a busy connection here
//! can return `SQLITE_BUSY` in well under a millisecond, nowhere near the
//! configured 5s timeout window - `busy_timeout` bounds how long one
//! blocked lock acquisition retries internally, it doesn't guarantee a
//! contending connection eventually wins the lock before giving up.
//!
//! This test manufactures the same race deterministically and with far
//! higher pressure than two polling subprocesses ever could - many
//! threads, each with its own connection, hammering `try_lock_instance`
//! back-to-back against one file - so it reproduces the bug on essentially
//! every run instead of relying on incidental process-scheduling timing.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use zen_runtime::durable_journal::SqliteJournal;
use zen_runtime::journal::{InstanceId, Journal};

#[test]
fn many_threads_hammering_try_lock_instance_never_surfaces_a_raw_busy_error() {
    let path = std::env::temp_dir()
        .join(format!("zen-runtime-try-lock-instance-stress-{}.db", std::process::id()))
        .to_string_lossy()
        .into_owned();
    let _ = std::fs::remove_file(&path);

    const THREADS: usize = 16;
    const ATTEMPTS_PER_THREAD: usize = 50;

    let errors = Arc::new(AtomicUsize::new(0));
    let handles: Vec<_> = (0..THREADS)
        .map(|thread_index| {
            let path = path.clone();
            let errors = Arc::clone(&errors);
            std::thread::spawn(move || {
                for attempt in 0..ATTEMPTS_PER_THREAD {
                    // A fresh connection per attempt, matching how a real
                    // `--watch` poll loop opens a new `SqliteJournal` each
                    // cycle - and a distinct instance id per attempt, so
                    // any error here is raw SQLite contention, never the
                    // legitimate "another live process holds this exact
                    // instance's lock" path.
                    let instance = InstanceId(format!("instance-{thread_index}-{attempt}"));
                    let opened = SqliteJournal::open(&path, InstanceId(instance.0.clone()));
                    match opened {
                        Ok(mut journal) => {
                            if let Err(error) = journal.try_lock_instance(&instance) {
                                eprintln!("try_lock_instance unexpectedly failed: {error}");
                                errors.fetch_add(1, Ordering::SeqCst);
                            }
                        }
                        Err(error) => {
                            eprintln!("SqliteJournal::open unexpectedly failed: {error}");
                            errors.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("worker thread panicked");
    }

    let _ = std::fs::remove_file(&path);
    assert_eq!(
        errors.load(Ordering::SeqCst),
        0,
        "expected try_lock_instance to transparently retry through SQLITE_BUSY under real concurrent access, not surface it"
    );
}
