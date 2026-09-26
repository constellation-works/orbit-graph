use std::fs::{self, File};
use std::time::{Duration, Instant};

use super::{FileLockGuard, LockHolder, read_holder};

#[test]
fn holder_record_names_the_process_after_acquire() {
    let dir = tempfile::tempdir().expect("create lock dir");
    let path = dir.path().join("graph.db.lock");

    // A concurrent test's fork briefly shares any open descriptor until its
    // exec, so even a free lock gets a short wait rather than none.
    let wait = Duration::from_secs(10);
    let guard = FileLockGuard::acquire(&path, "unit holder", wait, "lock test")
        .expect("free lock is acquired");

    let holder = read_holder(&path).expect("holder record is readable");
    assert_eq!(holder.pid, std::process::id());
    assert_eq!(holder.label, "unit holder");
    assert!(holder.acquired_at.ends_with('Z'), "{}", holder.acquired_at);
    drop(guard);

    let _next = FileLockGuard::acquire(&path, "next holder", wait, "lock test")
        .expect("dropping the guard releases the lock");
    assert_eq!(read_holder(&path).expect("new record").label, "next holder");
}

#[test]
fn contended_lock_times_out_naming_the_holder() {
    let dir = tempfile::tempdir().expect("create lock dir");
    let path = dir.path().join("graph.db.lock");
    let _held = FileLockGuard::acquire(&path, "stuck sync", Duration::from_secs(10), "lock test")
        .expect("first acquire");
    let holder = read_holder(&path).expect("holder record");

    let timeout = Duration::from_millis(40);
    let started = Instant::now();
    let error = FileLockGuard::acquire(&path, "waiter", timeout, "lock test")
        .expect_err("held lock must time out");
    let waited = started.elapsed();

    assert!(
        waited >= timeout,
        "returned before the deadline: {waited:?}"
    );
    assert!(
        waited < Duration::from_secs(5),
        "overran the deadline: {waited:?}"
    );
    let message = error.to_string();
    assert!(message.contains("timed out after 40 ms"), "{message}");
    assert!(
        message.contains(&format!("pid {}", holder.pid)),
        "{message}"
    );
    assert!(message.contains(&holder.acquired_at), "{message}");
    assert!(message.contains("stuck sync"), "{message}");
}

#[test]
fn refusal_without_a_readable_holder_is_contention() {
    let dir = tempfile::tempdir().expect("create lock dir");
    let path = dir.path().join("graph.db.lock");
    // Hold the kernel lock without writing a record, like an older binary.
    let raw = File::create(&path).expect("create lock file");
    raw.lock().expect("take raw lock");
    fs::write(&path, b"not a record").expect("write garbage record");

    let error = FileLockGuard::acquire(&path, "waiter", Duration::ZERO, "lock test")
        .expect_err("a refusal without a record is still contention");

    let message = error.to_string();
    assert!(message.contains("holder unknown"), "{message}");
    assert!(read_holder(&path).is_none());
}

#[test]
fn holder_display_names_pid_time_and_label() {
    let holder = LockHolder {
        pid: 42,
        acquired_at: "2026-09-26T00:00:00.000000000Z".to_string(),
        label: "orbit-graph graph sync".to_string(),
    };
    assert_eq!(
        holder.to_string(),
        "held by pid 42 since 2026-09-26T00:00:00.000000000Z (orbit-graph graph sync)"
    );
}
