use std::fs::{self, File};
use std::time::{Duration, Instant};

use super::{FileLockGuard, LockHolder, read_holder};

#[test]
fn retained_cleanup_probe_descriptor_is_released_on_unwind() {
    let dir = tempfile::tempdir().expect("create lock dir");
    let path = dir.path().join("graph.db.close.lock");
    fs::write(&path, b"unchanged holder").expect("plant holder record");
    // The plan path takes a lock on a read-only descriptor without writing.
    let raw = File::open(&path).expect("open cleanup probe");
    raw.try_lock().expect("lock cleanup probe");
    let retained = raw.try_clone().expect("retain probe descriptor");
    let result = std::panic::catch_unwind(|| {
        let _probe = FileLockGuard::from_locked(raw);
        assert!(matches!(
            File::open(&path).expect("open waiter").try_lock(),
            Err(fs::TryLockError::WouldBlock)
        ));
        panic!("unwind while cleanup owns the probe");
    });
    assert_eq!(
        result
            .expect_err("the probe scope unwinds")
            .downcast_ref::<&str>(),
        Some(&"unwind while cleanup owns the probe")
    );
    assert_eq!(fs::read(&path).expect("read holder"), b"unchanged holder");
    let next = File::open(&path).expect("open next probe");
    next.try_lock()
        .expect("unwinding releases the retained probe");
    next.unlock().expect("release next probe");
    assert!(retained.metadata().is_ok(), "the duplicate is still open");
}

#[test]
fn retained_sidecar_descriptor_does_not_extend_guard_lifetime() {
    let dir = tempfile::tempdir().expect("create lock dir");
    let path = dir.path().join("graph.db.lock");
    let guard = FileLockGuard::acquire(&path, "owner", Duration::ZERO, "lock test")
        .expect("acquire free lock");
    // try_clone retains the same open-file-description, just as fork does,
    // without a child process, scheduler timing, or a sleep.
    let retained = guard._file.try_clone().expect("retain owner's descriptor");
    let error = FileLockGuard::acquire(&path, "waiter", Duration::ZERO, "lock test")
        .expect_err("the live owner must exclude independent acquisitions");
    assert!(matches!(error, crate::GraphError::Timeout { .. }));

    drop(guard);
    let next = FileLockGuard::acquire(&path, "next owner", Duration::ZERO, "lock test")
        .expect("owner drop must release the lock while its duplicate remains open");
    assert!(retained.metadata().is_ok(), "the duplicate is still open");
    drop(retained);
    let error = FileLockGuard::acquire(&path, "waiter", Duration::ZERO, "lock test")
        .expect_err("closing the old duplicate must not unlock the new owner");
    assert!(matches!(error, crate::GraphError::Timeout { .. }));
    drop(next);
}

#[test]
fn retained_observation_descriptor_does_not_block_cleanup_after_guard_drop() {
    let repo = tempfile::tempdir().expect("create repository");
    crate::tests::support::set_discovery_boundary(repo.path());
    let stale = crate::resolve_db_path(repo.path(), "old", crate::EXTRACTOR_VERSION - 1)
        .path()
        .to_path_buf();
    crate::state_dir::create_private_dir_all(stale.parent().expect("graph directory"))
        .expect("create graph directory");
    crate::state_dir::write_new_private_file(&stale, b"old database")
        .expect("plant stale database");

    let observation = crate::store::acquire_observation_lock(&stale, Duration::ZERO)
        .expect("acquire directory observation guard");
    let retained = observation
        ._file
        .try_clone()
        .expect("retain observation descriptor");
    let error = crate::store::acquire_observation_lock(&stale, Duration::ZERO)
        .expect_err("a live observation guard must exclude another observer");
    assert!(matches!(error, crate::GraphError::Timeout { .. }));
    let report = crate::clean_old_databases(repo.path()).expect("clean while observation is held");
    assert!(report.deleted.is_empty());
    assert!(
        report
            .kept
            .iter()
            .any(|item| item.reason == crate::CleanReason::Locked)
    );
    assert_eq!(
        fs::read(&stale).expect("held database remains"),
        b"old database"
    );

    drop(observation);
    let report =
        crate::clean_old_databases(repo.path()).expect("clean after observation owner drops");
    assert!(
        !stale.exists(),
        "the released old DB must be removed while the observation duplicate remains open: {report:?}"
    );
    assert_eq!(
        report.deleted.len(),
        2,
        "database and existing sync sidecar"
    );
    assert!(retained.metadata().is_ok(), "the duplicate is still open");
    drop(retained);
}

#[test]
fn holder_record_names_the_process_after_acquire() {
    let dir = tempfile::tempdir().expect("create lock dir");
    let path = dir.path().join("graph.db.lock");

    let wait = Duration::ZERO;
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
